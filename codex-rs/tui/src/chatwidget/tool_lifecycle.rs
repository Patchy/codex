//! Non-command tool lifecycle rendering for `ChatWidget`.
//!
//! This module handles patch, MCP, web search, image, and collaborator tool
//! events as transcript cells.

use super::*;
use crate::thread_transcript::tools::McpHistory;
use codex_utils_path_uri::LegacyAppPathString;

impl ChatWidget {
    pub(super) fn on_patch_apply_begin(&mut self, changes: HashMap<PathBuf, FileChange>) {
        self.add_to_history(history_cell::new_patch_event(changes, &self.config.cwd));
    }

    pub(super) fn on_view_image_tool_call(&mut self, path: LegacyAppPathString) {
        self.flush_answer_stream_with_separator();
        self.add_to_history(history_cell::new_view_image_tool_call(path));
        self.request_redraw();
    }

    pub(super) fn on_image_generation_begin(&mut self) {
        self.flush_answer_stream_with_separator();
        if self.bottom_pane.is_task_running() {
            self.bottom_pane.ensure_status_indicator();
        }
    }

    pub(super) fn on_image_generation_end(
        &mut self,
        call_id: String,
        status: String,
        revised_prompt: Option<String>,
        saved_path: Option<AbsolutePathBuf>,
    ) {
        self.flush_answer_stream_with_separator();
        self.add_to_history(history_cell::new_image_generation_call(
            call_id,
            &status,
            revised_prompt,
            saved_path,
        ));
        self.request_redraw();
    }

    pub(super) fn on_file_change_completed(&mut self, item: ThreadItem) {
        self.defer_or_handle(
            item,
            InterruptManager::push_item_completed,
            Self::handle_file_change_completed_now,
        );
    }

    pub(super) fn on_mcp_tool_call_started(&mut self, item: ThreadItem) {
        self.defer_or_handle(
            item,
            InterruptManager::push_item_started,
            Self::handle_mcp_tool_call_started_now,
        );
    }

    pub(super) fn on_mcp_tool_call_completed(&mut self, item: ThreadItem) {
        self.defer_or_handle(
            item,
            InterruptManager::push_item_completed,
            Self::handle_mcp_tool_call_completed_now,
        );
    }

    pub(super) fn on_web_search_begin(&mut self, call_id: String) {
        self.flush_answer_stream_with_separator();
        self.flush_active_cell();
        self.transcript.active_cell = Some(Box::new(history_cell::new_active_web_search_call(
            call_id,
            String::new(),
            self.local_settings.tui.animations && self.local_settings.tui.effects.progress,
        )));
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    pub(super) fn on_web_search_end(
        &mut self,
        call_id: String,
        query: String,
        action: codex_app_server_protocol::WebSearchAction,
    ) {
        self.flush_answer_stream_with_separator();
        let mut handled = false;
        if let Some(cell) = self
            .transcript
            .active_cell
            .as_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<WebSearchCell>())
            && cell.call_id() == call_id
        {
            cell.update(action.clone(), query.clone());
            cell.complete();
            self.bump_active_cell_revision();
            self.flush_active_cell();
            handled = true;
        }

        if !handled {
            self.add_to_history(history_cell::new_web_search_call(call_id, query, action));
        }
    }

    pub(super) fn on_collab_event(&mut self, cell: PlainHistoryCell) {
        self.flush_answer_stream_with_separator();
        self.add_to_history(cell);
        self.request_redraw();
    }

    pub(super) fn on_collab_agent_tool_call(&mut self, item: ThreadItem, update_panel: bool) {
        let ThreadItem::CollabAgentToolCall {
            id, tool, status, ..
        } = &item
        else {
            return;
        };
        if matches!(tool, CollabAgentTool::SpawnAgent)
            && let Some(spawn_request) = multi_agents::spawn_request_summary(&item)
        {
            self.pending_collab_spawn_requests
                .insert(id.clone(), spawn_request);
        }

        let cached_spawn_request = if matches!(tool, CollabAgentTool::SpawnAgent)
            && !matches!(status, CollabAgentToolCallStatus::InProgress)
        {
            self.pending_collab_spawn_requests.remove(id)
        } else {
            None
        };

        if let Some(cell) = multi_agents::tool_call_history_cell(
            &item,
            cached_spawn_request.as_ref(),
            |thread_id| self.collab_agent_metadata(thread_id),
        ) {
            self.on_collab_event(cell);
        }

        if update_panel {
            self.update_subagent_panel_from_tool_call(&item);
        }
    }

    /// Applies a completed collab tool call to the live subagent panel.
    fn update_subagent_panel_from_tool_call(&mut self, item: &ThreadItem) {
        use crate::subagent_panel::PanelAgentStatus;

        let ThreadItem::CollabAgentToolCall {
            tool,
            status,
            receiver_thread_ids,
            prompt,
            agents_states,
            ..
        } = item
        else {
            return;
        };
        // Receiver thread ids and per-agent states are only reliable once the
        // tool call resolves; the InProgress notification precedes them.
        if matches!(status, CollabAgentToolCallStatus::InProgress) {
            return;
        }

        let first_receiver = receiver_thread_ids
            .first()
            .and_then(|thread_id| ThreadId::from_string(thread_id).ok());

        match tool {
            CollabAgentTool::SpawnAgent => {
                if let Some(receiver_thread_id) = first_receiver {
                    let metadata = self.collab_agent_metadata(receiver_thread_id);
                    let status = agents_states
                        .get(&receiver_thread_id.to_string())
                        .map(PanelAgentStatus::from_collab_state)
                        .unwrap_or(PanelAgentStatus::PendingInit);
                    self.subagent_panel_registry.on_spawn(
                        receiver_thread_id,
                        metadata.agent_nickname,
                        metadata.agent_role,
                        prompt.as_deref().unwrap_or_default(),
                        status,
                    );
                    self.refresh_subagent_panel();
                }
            }
            CollabAgentTool::SendInput
            | CollabAgentTool::ResumeAgent
            | CollabAgentTool::SendMessage
            | CollabAgentTool::FollowupTask => {
                if let Some(receiver_thread_id) = first_receiver {
                    let status = agents_states
                        .get(&receiver_thread_id.to_string())
                        .map(PanelAgentStatus::from_collab_state)
                        .unwrap_or_else(|| {
                            PanelAgentStatus::Errored("Agent interaction failed".into())
                        });
                    self.subagent_panel_registry
                        .update_status(receiver_thread_id, status);
                    self.refresh_subagent_panel();
                }
            }
            CollabAgentTool::Wait => {
                for receiver_thread_id in receiver_thread_ids {
                    if let Ok(thread_id) = ThreadId::from_string(receiver_thread_id)
                        && let Some(status) = agents_states
                            .get(receiver_thread_id)
                            .map(PanelAgentStatus::from_collab_state)
                    {
                        self.subagent_panel_registry
                            .update_status(thread_id, status);
                    }
                }
                self.refresh_subagent_panel();
            }
            CollabAgentTool::CloseAgent => {
                if let Some(receiver_thread_id) = first_receiver {
                    self.subagent_panel_registry.close(receiver_thread_id);
                    self.refresh_subagent_panel();
                }
            }
            CollabAgentTool::InterruptAgent => {
                if let Some(receiver_thread_id) = first_receiver {
                    self.subagent_panel_registry
                        .update_status(receiver_thread_id, PanelAgentStatus::Interrupted);
                    self.refresh_subagent_panel();
                }
            }
            CollabAgentTool::ListAgents => {}
        }
    }

    pub(super) fn refresh_subagent_panel(&mut self) {
        self.subagent_panel = self.subagent_panel_registry.rebuild_panel();
        self.request_redraw();
    }

    pub(super) fn on_sub_agent_activity(&mut self, item: ThreadItem, update_panel: bool) {
        // Replayed activity renders history only; it must never mount the
        // live panel for agents that finished in a previous session.
        if !update_panel {
            if let Some(cell) = multi_agents::sub_agent_activity_history_cell(&item) {
                self.on_collab_event(cell);
            }
            return;
        }
        // Background agents can finish while the parent answer is still streaming.
        // Keep that stream intact until its authoritative message completion.
        // After the turn stops, leftover prompts must not hold up late activity.
        if !self.turn_lifecycle.agent_turn_running && self.stream_controller.is_none() {
            self.handle_sub_agent_activity_now(item);
        } else {
            self.defer_or_handle(
                item,
                InterruptManager::push_item_completed,
                Self::handle_sub_agent_activity_now,
            );
        }
    }

    fn handle_sub_agent_activity_now(&mut self, item: ThreadItem) {
        if let Some(cell) = multi_agents::sub_agent_activity_history_cell(&item) {
            self.on_collab_event(cell);
        }

        if let ThreadItem::SubAgentActivity {
                kind,
                agent_thread_id,
                agent_path,
                ..
            } = &item
            && let Ok(thread_id) = ThreadId::from_string(agent_thread_id)
        {
            use codex_app_server_protocol::SubAgentActivityKind;
            match kind {
                // Under v2, Started is the only spawn signal; register the
                // agent here. Nickname/role arrive via ThreadStarted metadata
                // and may lag the first activity, so re-read on every event.
                SubAgentActivityKind::Started | SubAgentActivityKind::Interacted => {
                    let metadata = self.collab_agent_metadata(thread_id);
                    self.subagent_panel_registry.upsert_activity(
                        thread_id,
                        agent_path,
                        metadata.agent_nickname,
                        metadata.agent_role,
                        format!("working in `{agent_path}`"),
                    );
                }
                SubAgentActivityKind::Interrupted => {
                    self.subagent_panel_registry.update_status(
                        thread_id,
                        crate::subagent_panel::PanelAgentStatus::Interrupted,
                    );
                }
                SubAgentActivityKind::Completed => {
                    self.subagent_panel_registry.update_status(
                        thread_id,
                        crate::subagent_panel::PanelAgentStatus::Completed(None),
                    );
                }
            }
            self.refresh_subagent_panel();
        }
    }

    pub(crate) fn handle_file_change_completed_now(&mut self, item: ThreadItem) {
        let ThreadItem::FileChange { status, .. } = item else {
            return;
        };
        // If the patch was successful, just let the "Edited" block stand.
        // Otherwise, add a failure block.
        if matches!(status, codex_app_server_protocol::PatchApplyStatus::Failed) {
            self.add_to_history(history_cell::new_patch_apply_failure(String::new()));
        }
    }

    pub(crate) fn handle_mcp_tool_call_started_now(&mut self, item: ThreadItem) {
        let ThreadItem::McpToolCall {
            id,
            server,
            tool,
            arguments,
            ..
        } = item
        else {
            return;
        };
        self.flush_answer_stream_with_separator();
        let invocation = McpInvocation {
            server,
            tool,
            arguments: Some(arguments),
        };
        if invocation.is_computer_activity() {
            let call = history_cell::new_active_mcp_tool_call(
                id,
                invocation,
                self.local_settings.tui.animations && self.local_settings.tui.effects.progress,
            );
            self.update_computer_activity(|cell| cell.start(call));
            self.bump_active_cell_revision();
            self.request_redraw();
            return;
        }
        self.flush_active_cell();
        self.transcript.active_cell = Some(Box::new(history_cell::new_active_mcp_tool_call(
            id,
            invocation,
            self.local_settings.tui.animations && self.local_settings.tui.effects.progress,
        )));
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    pub(crate) fn handle_mcp_tool_call_completed_now(&mut self, item: ThreadItem) {
        self.flush_answer_stream_with_separator();

        let Some(McpHistory {
            id,
            invocation,
            duration,
            result,
        }) = McpHistory::from_item(item)
        else {
            return;
        };

        if invocation.is_computer_activity() {
            let call = history_cell::new_active_mcp_tool_call(
                id,
                invocation,
                self.local_settings.tui.animations && self.local_settings.tui.effects.progress,
            );
            self.update_computer_activity(|cell| cell.complete(call, duration, result));
            self.bump_active_cell_revision();
            self.request_redraw();
            return;
        }

        match self
            .transcript
            .active_cell
            .as_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<McpToolCallCell>())
        {
            Some(cell) if cell.call_id() == id => cell.complete(duration, result),
            _ => {
                self.flush_active_cell();
                let mut cell = history_cell::new_active_mcp_tool_call(
                    id,
                    invocation,
                    self.local_settings.tui.animations && self.local_settings.tui.effects.progress,
                );
                cell.complete(duration, result);
                self.transcript.active_cell = Some(Box::new(cell));
            }
        };

        self.flush_active_cell();
    }

    /// Preserve pending calls and timers, returning the revisions before and after hydration.
    pub(crate) fn prepend_active_computer_history(
        &mut self,
        older: &dyn HistoryCell,
        turns: &[Turn],
    ) -> Option<(u64, u64)> {
        let previous_revision = self.transcript.active_cell_revision;
        let active = self.transcript.active_cell.as_mut().and_then(|cell| {
            cell.as_any_mut()
                .downcast_mut::<history_cell::ComputerActivityCell>()
        })?;
        let older = crate::thread_transcript::older_computer_group(older, active, turns)?;
        active.prepend(older);
        self.bump_active_cell_revision();
        Some((previous_revision, self.transcript.active_cell_revision))
    }

    /// Reuse only adjacent computer calls; all other active cells form a transcript boundary.
    fn update_computer_activity(
        &mut self,
        update: impl FnOnce(&mut history_cell::ComputerActivityCell),
    ) {
        if let Some(cell) = self.transcript.active_cell.as_mut().and_then(|cell| {
            cell.as_any_mut()
                .downcast_mut::<history_cell::ComputerActivityCell>()
        }) {
            update(cell);
        } else {
            self.flush_active_cell();
            let mut cell = history_cell::ComputerActivityCell::default();
            update(&mut cell);
            self.transcript.active_cell = Some(Box::new(cell));
        }
    }

    pub(crate) fn handle_queued_item_started_now(&mut self, item: ThreadItem) {
        match item {
            item @ ThreadItem::CommandExecution { .. } => {
                self.handle_command_execution_started_now(item);
            }
            item @ ThreadItem::McpToolCall { .. } => {
                self.handle_mcp_tool_call_started_now(item);
            }
            item @ ThreadItem::DynamicToolCall { .. } => self.handle_dynamic_tool_item_now(item),
            _ => {}
        }
    }

    pub(crate) fn handle_queued_item_completed_now(&mut self, item: ThreadItem) {
        match item {
            item @ ThreadItem::CommandExecution { .. } => {
                self.handle_command_execution_completed_now(item);
            }
            item @ ThreadItem::FileChange { .. } => self.handle_file_change_completed_now(item),
            item @ ThreadItem::McpToolCall { .. } => self.handle_mcp_tool_call_completed_now(item),
            item @ ThreadItem::DynamicToolCall { .. } => self.handle_dynamic_tool_item_now(item),
            item @ ThreadItem::SubAgentActivity { .. } => self.handle_sub_agent_activity_now(item),
            _ => {}
        }
    }
}
