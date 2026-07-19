//! Live subagent panel pinned above the composer.
//!
//! Tracks subagent threads observed via `CollabAgentToolCall` /
//! `SubAgentActivity` thread items and renders one always-visible row per
//! active agent: status, name, elapsed time, and a preview of the latest
//! activity. Ported from the unmerged upstream branch
//! `dev/friel/frodex-129-tui-subagents-fork` and adapted to the current
//! app-server protocol (`CollabAgentState` instead of core `AgentStatus`).

use crate::history_cell::HistoryCell;
use crate::motion::MotionMode;
use crate::motion::shimmer_text;
use crate::status_indicator_widget::fmt_elapsed_compact;
use crate::text_formatting::truncate_text;
use codex_app_server_protocol::CollabAgentState;
use codex_app_server_protocol::CollabAgentStatus;
use codex_protocol::ThreadId;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

const SUBAGENT_PROMPT_PREVIEW_BUDGET: usize = 120;
const SUBAGENT_UPDATE_PREVIEW_BUDGET: usize = 160;
const SUBAGENT_SHIMMER_WINDOW: Duration = Duration::from_secs(1);

/// Panel-local agent status derived from `CollabAgentState`, so the panel does
/// not depend on the core `AgentStatus` type that never reaches the TUI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PanelAgentStatus {
    PendingInit,
    Running,
    Interrupted,
    Completed(Option<String>),
    Errored(String),
    Shutdown,
    NotFound,
}

impl PanelAgentStatus {
    pub(crate) fn from_collab_state(state: &CollabAgentState) -> Self {
        match state.status {
            CollabAgentStatus::PendingInit => Self::PendingInit,
            CollabAgentStatus::Running => Self::Running,
            CollabAgentStatus::Interrupted => Self::Interrupted,
            CollabAgentStatus::Completed => Self::Completed(state.message.clone()),
            CollabAgentStatus::Errored => Self::Errored(
                state
                    .message
                    .clone()
                    .unwrap_or_else(|| "Agent failed".to_string()),
            ),
            CollabAgentStatus::Shutdown => Self::Shutdown,
            CollabAgentStatus::NotFound => Self::NotFound,
        }
    }

    fn is_running(&self) -> bool {
        matches!(self, Self::PendingInit | Self::Running)
    }
}

#[derive(Debug, Clone)]
struct SubagentInfo {
    ordinal: i32,
    name: String,
    role: Option<String>,
    prompt_preview: String,
    status: PanelAgentStatus,
    spawned_at: Instant,
    latest_preview: String,
    latest_update_at: Instant,
}

impl SubagentInfo {
    fn new(ordinal: i32, name: String, role: Option<String>, prompt: &str) -> Self {
        let now = Instant::now();
        let prompt_preview = prompt_preview(prompt);
        Self {
            ordinal,
            name,
            role,
            prompt_preview: prompt_preview.clone(),
            status: PanelAgentStatus::PendingInit,
            spawned_at: now,
            latest_preview: prompt_preview,
            latest_update_at: now,
        }
    }

    fn is_watchdog(&self) -> bool {
        self.role.as_deref() == Some("watchdog")
    }

    fn is_visible_in_panel(&self) -> bool {
        self.status.is_running()
    }

    fn is_running_for_panel(&self) -> bool {
        if self.is_watchdog() {
            return matches!(self.status, PanelAgentStatus::Running);
        }
        self.status.is_running()
    }

    fn update_status(&mut self, status: PanelAgentStatus) {
        self.latest_preview =
            status_preview(&status).unwrap_or_else(|| self.prompt_preview.clone());
        self.status = status;
        self.latest_update_at = Instant::now();
    }

    fn note_activity(&mut self, preview: String) {
        if !preview.is_empty() {
            self.latest_preview = truncate_text(&preview, SUBAGENT_UPDATE_PREVIEW_BUDGET);
        }
        self.latest_update_at = Instant::now();
    }
}

#[derive(Debug, Default)]
pub(crate) struct SubagentPanelRegistry {
    agents: HashMap<ThreadId, SubagentInfo>,
    order: Vec<ThreadId>,
    panel_state: Option<Arc<Mutex<SubagentPanelState>>>,
    motion_mode: Option<MotionMode>,
}

impl SubagentPanelRegistry {
    pub(crate) fn new(motion_mode: MotionMode) -> Self {
        Self {
            motion_mode: Some(motion_mode),
            ..Self::default()
        }
    }

    pub(crate) fn on_spawn(
        &mut self,
        thread_id: ThreadId,
        nickname: Option<String>,
        role: Option<String>,
        prompt: &str,
        status: PanelAgentStatus,
    ) {
        if role.as_deref() == Some("watchdog") {
            self.prune_superseded_watchdogs(thread_id);
        }

        let ordinal = self.ordinal_for(thread_id);
        let name = nickname
            .filter(|nickname| !nickname.trim().is_empty())
            .unwrap_or_else(|| derive_subagent_name(prompt, ordinal));

        let info = self.agents.entry(thread_id).or_insert_with(|| {
            self.order.push(thread_id);
            SubagentInfo::new(ordinal, name.clone(), role.clone(), prompt)
        });
        info.name = name;
        info.role = role;
        info.update_status(status);
    }

    pub(crate) fn update_status(&mut self, thread_id: ThreadId, status: PanelAgentStatus) {
        if let Some(info) = self.agents.get_mut(&thread_id) {
            info.update_status(status);
        }
    }

    /// Registers-or-updates an agent from a v2 `SubAgentActivity` item, which
    /// is the only spawn signal the canonical v2 path emits (no
    /// `CollabAgentToolCall{SpawnAgent}` fires under `multi_agent_v2`).
    pub(crate) fn upsert_activity(
        &mut self,
        thread_id: ThreadId,
        agent_path: &str,
        nickname: Option<String>,
        role: Option<String>,
        preview: String,
    ) {
        let ordinal = self.ordinal_for(thread_id);
        let display_name = nickname
            .filter(|nickname| !nickname.trim().is_empty())
            .unwrap_or_else(|| agent_path.to_string());
        let info = self.agents.entry(thread_id).or_insert_with(|| {
            self.order.push(thread_id);
            SubagentInfo::new(ordinal, display_name.clone(), role.clone(), agent_path)
        });
        info.name = display_name;
        if role.is_some() {
            info.role = role;
        }
        info.note_activity(preview);
    }

    /// Applies thread-level liveness (TurnStarted/TurnCompleted/ThreadClosed)
    /// to a tracked agent. Under v2 this is the only completion signal: no
    /// tool item ever reports a v2 agent finishing.
    pub(crate) fn set_thread_running(&mut self, thread_id: ThreadId, running: bool) {
        if let Some(info) = self.agents.get_mut(&thread_id) {
            let status = if running {
                PanelAgentStatus::Running
            } else {
                PanelAgentStatus::Completed(None)
            };
            // A wait/close result may carry a richer terminal status
            // (errored, interrupted, message); never downgrade those.
            if info.status.is_running() || running {
                info.update_status(status);
            }
        }
    }

    pub(crate) fn close(&mut self, thread_id: ThreadId) {
        self.agents.remove(&thread_id);
        self.order.retain(|candidate| *candidate != thread_id);
    }

    /// Rebuilds the shared panel state and returns a cell to pin, or `None`
    /// when no agents are visible (panel should unmount).
    pub(crate) fn rebuild_panel(&mut self) -> Option<SubagentStatusCell> {
        let mut visible = self
            .order
            .iter()
            .filter_map(|thread_id| self.agents.get(thread_id))
            .filter(|info| info.is_visible_in_panel())
            .collect::<Vec<_>>();
        visible.sort_by_key(|info| info.ordinal);

        if visible.is_empty() {
            self.panel_state = None;
            return None;
        }

        let started_at = visible
            .iter()
            .map(|info| info.spawned_at)
            .min()
            .unwrap_or_else(Instant::now);
        let running_count = i32::try_from(
            visible
                .iter()
                .filter(|info| info.is_running_for_panel())
                .count(),
        )
        .unwrap_or(i32::MAX);
        let total_agents = i32::try_from(visible.len()).unwrap_or(i32::MAX);
        let running_agents = visible
            .into_iter()
            .map(|info| SubagentPanelAgent {
                ordinal: info.ordinal,
                name: info.name.clone(),
                status: info.status.clone(),
                is_watchdog: info.is_watchdog(),
                preview: info.latest_preview.clone(),
                spawned_at: info.spawned_at,
                latest_update_at: info.latest_update_at,
            })
            .collect();
        let state = SubagentPanelState {
            started_at,
            total_agents,
            running_count,
            running_agents,
        };

        match &self.panel_state {
            Some(existing) => {
                let mut guard = existing
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *guard = state;
            }
            None => {
                self.panel_state = Some(Arc::new(Mutex::new(state)));
            }
        }

        self.panel_state.as_ref().map(|state| SubagentStatusCell {
            state: Arc::clone(state),
            motion_mode: self.motion_mode.unwrap_or(MotionMode::Reduced),
        })
    }

    fn ordinal_for(&self, thread_id: ThreadId) -> i32 {
        if let Some(existing) = self.agents.get(&thread_id) {
            return existing.ordinal;
        }
        i32::try_from(self.order.len())
            .unwrap_or(i32::MAX - 1)
            .saturating_add(1)
    }

    fn prune_superseded_watchdogs(&mut self, keep_thread_id: ThreadId) {
        let superseded = self
            .agents
            .iter()
            .filter_map(|(thread_id, info)| {
                (info.is_watchdog() && *thread_id != keep_thread_id).then_some(*thread_id)
            })
            .collect::<Vec<_>>();
        for thread_id in superseded {
            self.close(thread_id);
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SubagentPanelAgent {
    pub(crate) ordinal: i32,
    pub(crate) name: String,
    pub(crate) status: PanelAgentStatus,
    pub(crate) is_watchdog: bool,
    pub(crate) preview: String,
    pub(crate) spawned_at: Instant,
    pub(crate) latest_update_at: Instant,
}

#[derive(Clone, Debug)]
pub(crate) struct SubagentPanelState {
    pub(crate) started_at: Instant,
    pub(crate) total_agents: i32,
    pub(crate) running_count: i32,
    pub(crate) running_agents: Vec<SubagentPanelAgent>,
}

impl SubagentPanelState {
    fn has_animating_agents(&self, now: Instant) -> bool {
        self.running_agents
            .iter()
            .any(|agent| should_shimmer(agent, now))
    }
}

/// Pinned live panel cell. Shares state with the registry via `Arc<Mutex<_>>`
/// so updates render without remounting the cell.
#[derive(Clone, Debug)]
pub(crate) struct SubagentStatusCell {
    state: Arc<Mutex<SubagentPanelState>>,
    motion_mode: MotionMode,
}

impl HistoryCell for SubagentStatusCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let state = {
            let guard = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.clone()
        };
        if state.running_agents.is_empty() {
            return Vec::new();
        }

        let elapsed = fmt_elapsed_compact(state.started_at.elapsed().as_secs());
        let total_agents = state.total_agents.max(state.running_count);
        let count_label = subagent_count_label(total_agents, state.running_count);
        let header_suffix = format!("({elapsed} • {count_label} • esc to interrupt)");

        let mut lines = Vec::new();
        lines.push(Line::from(vec![
            "• ".dim(),
            "Subagents".bold(),
            " ".into(),
            header_suffix.dim(),
        ]));

        let mut running_agents = state.running_agents;
        running_agents.sort_by_key(|agent| agent.ordinal);
        let preview_budget = running_preview_budget(width);
        let now = Instant::now();
        lines.extend(running_agents.into_iter().map(|agent| {
            let preview = truncate_text(agent.preview.trim(), preview_budget);
            let agent_elapsed = fmt_elapsed_compact(agent.spawned_at.elapsed().as_secs());
            let mut spans: Vec<Span<'static>> =
                vec!["  • ".dim(), format!("[#{}] ", agent.ordinal).dim()];
            if agent.is_watchdog {
                spans.push("[watchdog] ".magenta().dim());
            }
            spans.push(Span::from(agent.name.clone()));
            spans.push(" ".into());
            spans.push(status_span(&agent));
            spans.push(format!(" · {agent_elapsed}").dim());
            spans.push(" — ".dim());
            if should_shimmer(&agent, now) {
                spans.extend(shimmer_text(&preview, self.motion_mode));
            } else {
                spans.push(Span::from(preview).dim());
            }
            Line::from(spans)
        }));

        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(u16::MAX)
    }

    fn transcript_animation_tick(&self) -> Option<u64> {
        if matches!(self.motion_mode, MotionMode::Reduced) {
            return None;
        }
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if !guard.has_animating_agents(now) {
            return None;
        }
        Some((now.duration_since(guard.started_at).as_millis() / 100) as u64)
    }
}

impl crate::render::renderable::Renderable for SubagentStatusCell {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.display_lines(area.width);
        let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        let y = if area.height == 0 {
            0
        } else {
            let overflow = paragraph
                .line_count(area.width)
                .saturating_sub(usize::from(area.height));
            u16::try_from(overflow).unwrap_or(u16::MAX)
        };
        paragraph.scroll((y, 0)).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        HistoryCell::desired_height(self, width)
    }
}

fn running_preview_budget(width: u16) -> usize {
    let width = width as usize;
    width.saturating_sub(32).clamp(40, 160)
}

fn status_span(agent: &SubagentPanelAgent) -> Span<'static> {
    match &agent.status {
        PanelAgentStatus::PendingInit if agent.is_watchdog => "idle".dim(),
        PanelAgentStatus::PendingInit | PanelAgentStatus::Running => "running".cyan().bold(),
        PanelAgentStatus::Interrupted => "interrupted".magenta(),
        PanelAgentStatus::Completed(_) => "completed".green(),
        PanelAgentStatus::Errored(_) => "errored".red(),
        PanelAgentStatus::Shutdown => "shutdown".dim(),
        PanelAgentStatus::NotFound => "not found".red(),
    }
}

fn should_shimmer(agent: &SubagentPanelAgent, now: Instant) -> bool {
    if agent.is_watchdog && matches!(agent.status, PanelAgentStatus::PendingInit) {
        return false;
    }
    agent.status.is_running()
        && now.saturating_duration_since(agent.latest_update_at) <= SUBAGENT_SHIMMER_WINDOW
}

fn subagent_count_label(total: i32, running: i32) -> String {
    if total <= 0 || running <= 0 {
        return "no subagents running".to_string();
    }
    let total_label = subagent_pluralize(total, "subagent");
    if running >= total {
        return format!("{total_label} running");
    }
    format!("{total_label}, {running} running")
}

fn subagent_pluralize(count: i32, singular: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {singular}s")
    }
}

fn prompt_preview(prompt: &str) -> String {
    let first_line = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(prompt)
        .trim();
    truncate_text(first_line, SUBAGENT_PROMPT_PREVIEW_BUDGET)
}

fn derive_subagent_name(prompt: &str, ordinal: i32) -> String {
    let preview = prompt_preview(prompt);
    if preview.is_empty() {
        return format!("agent-{ordinal}");
    }
    preview
}

fn status_preview(status: &PanelAgentStatus) -> Option<String> {
    match status {
        PanelAgentStatus::Completed(Some(message)) | PanelAgentStatus::Errored(message) => Some(
            truncate_text(message.trim(), SUBAGENT_UPDATE_PREVIEW_BUDGET),
        ),
        PanelAgentStatus::Completed(None) => Some("completed".to_string()),
        PanelAgentStatus::Interrupted => Some("interrupted".to_string()),
        PanelAgentStatus::Shutdown => Some("shutdown".to_string()),
        PanelAgentStatus::NotFound => Some("not found".to_string()),
        PanelAgentStatus::PendingInit | PanelAgentStatus::Running => None,
    }
}
