//! Non-command tool lifecycle rendering for `ChatWidget`.
//!
//! This module handles patch, MCP, web search, image, and collaborator tool
//! events as transcript cells.

use super::*;
use codex_utils_path_uri::LegacyAppPathString;

impl ChatWidget {
    pub(super) fn on_patch_apply_begin(&mut self, changes: HashMap<PathBuf, FileChange>) {
        self.add_to_history(history_cell::new_patch_event(changes, &self.config.cwd));
    }

    pub(super) fn on_view_image_tool_call(&mut self, path: LegacyAppPathString) {
        self.flush_answer_stream_with_separator();
        self.add_to_history(history_cell::new_view_image_tool_call(
            path,
            &self.config.cwd,
        ));
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
            self.config.animations,
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
        self.transcript.had_work_activity = true;
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
            CollabAgentTool::SendInput | CollabAgentTool::ResumeAgent => {
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
        }
    }

    pub(super) fn refresh_subagent_panel(&mut self) {
        self.subagent_panel = self.subagent_panel_registry.rebuild_panel();
        self.request_redraw();
    }

    pub(super) fn on_sub_agent_activity(&mut self, item: ThreadItem, update_panel: bool) {
        if let Some(cell) = multi_agents::sub_agent_activity_history_cell(&item) {
            self.on_collab_event(cell);
        }

        if update_panel
            && let ThreadItem::SubAgentActivity {
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
        // Mark that actual work was done (patch applied)
        self.transcript.had_work_activity = true;
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
        self.flush_active_cell();
        self.transcript.active_cell = Some(Box::new(history_cell::new_active_mcp_tool_call(
            id,
            McpInvocation {
                server,
                tool,
                arguments: Some(arguments),
            },
            self.config.animations,
        )));
        self.bump_active_cell_revision();
        self.request_redraw();
    }

    pub(crate) fn handle_mcp_tool_call_completed_now(&mut self, item: ThreadItem) {
        self.flush_answer_stream_with_separator();

        let ThreadItem::McpToolCall {
            id,
            server,
            tool,
            arguments,
            result,
            error,
            duration_ms,
            ..
        } = item
        else {
            return;
        };
        let invocation = McpInvocation {
            server,
            tool,
            arguments: Some(arguments),
        };
        let duration = Duration::from_millis(duration_ms.unwrap_or_default().max(0) as u64);
        let result = match (result, error) {
            (_, Some(error)) => Err(error.message),
            (Some(result), None) => {
                let result = *result;
                Ok(codex_protocol::mcp::CallToolResult {
                    content: result.content,
                    structured_content: result.structured_content,
                    is_error: Some(false),
                    meta: None,
                })
            }
            (None, None) => Err("MCP tool call completed without a result".to_string()),
        };

        let extra_cell = match self
            .transcript
            .active_cell
            .as_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<McpToolCallCell>())
        {
            Some(cell) if cell.call_id() == id => cell.complete(duration, result),
            _ => {
                self.flush_active_cell();
                let mut cell =
                    history_cell::new_active_mcp_tool_call(id, invocation, self.config.animations);
                let extra_cell = cell.complete(duration, result);
                self.transcript.active_cell = Some(Box::new(cell));
                extra_cell
            }
        };

        self.flush_active_cell();
        if let Some(extra) = extra_cell {
            self.add_boxed_history(extra);
        }
        // Mark that actual work was done (MCP tool call)
        self.transcript.had_work_activity = true;
    }

    pub(crate) fn handle_queued_item_started_now(&mut self, item: ThreadItem) {
        match item {
            item @ ThreadItem::CommandExecution { .. } => {
                self.handle_command_execution_started_now(item);
            }
            item @ ThreadItem::McpToolCall { .. } => {
                self.handle_mcp_tool_call_started_now(item);
            }
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
            _ => {}
        }
    }
}
