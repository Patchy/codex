//! Full-screen agent dashboard (`/panel`, default keybind alt-a).
//!
//! Renders every agent seen this session — running and finished — with
//! per-agent stats, inside a pager overlay. Content is rebuilt from the
//! [`crate::subagent_panel::SubagentPanelRegistry`] snapshot on every draw so
//! the view stays live while open.

use crate::status_indicator_widget::fmt_elapsed_compact;
use crate::subagent_panel::PanelAgentStatus;
use crate::subagent_panel::SubagentStats;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use std::time::Instant;

pub(crate) const DASHBOARD_TITLE: &str = "Agents";

pub(crate) fn dashboard_lines(stats: &[SubagentStats]) -> Vec<Line<'static>> {
    let now = Instant::now();
    let mut lines: Vec<Line<'static>> = Vec::new();

    if stats.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            "  No subagents have run in this session yet.".dim(),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            "  Agents appear here as soon as the model spawns them.".dim(),
        ]));
        return lines;
    }

    let running = stats
        .iter()
        .filter(|stat| {
            matches!(
                stat.status,
                PanelAgentStatus::PendingInit | PanelAgentStatus::Running
            )
        })
        .count();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        "  ".into(),
        format!("{} agent(s) this session", stats.len()).bold(),
        format!(" · {running} running").dim(),
    ]));
    lines.push(Line::from(""));

    for stat in stats {
        let elapsed = fmt_elapsed_compact(stat.spawned_at.elapsed().as_secs());
        let idle = now
            .saturating_duration_since(stat.latest_update_at)
            .as_secs();

        let mut header: Vec<Span<'static>> = vec![
            "  ".into(),
            format!("#{} ", stat.ordinal).dim(),
            Span::from(stat.name.clone()).bold(),
        ];
        if let Some(role) = &stat.role {
            header.push(format!(" [{role}]").magenta().dim());
        }
        header.push(" ".into());
        header.push(status_span(&stat.status));
        lines.push(Line::from(header));

        let mut detail: Vec<Span<'static>> = vec!["     ".into()];
        detail.push(format!("age {elapsed}").dim());
        detail.push(format!(" · {} turn(s)", stat.turns_completed).dim());
        detail.push(format!(" · {} event(s)", stat.activity_count).dim());
        if idle >= 5 {
            detail.push(format!(" · last update {idle}s ago").dim());
        }
        detail.push(format!(" · thread {}", short_thread_id(&stat.thread_id.to_string())).dim());
        lines.push(Line::from(detail));

        if !stat.latest_preview.trim().is_empty() {
            lines.push(Line::from(vec![
                "     ".into(),
                "» ".dim(),
                Span::from(stat.latest_preview.clone()).italic().dim(),
            ]));
        }
        lines.push(Line::from(""));
    }

    lines.push(Line::from(vec![
        "  ".into(),
        "Alt+←/→ switches the transcript between agents · /agent opens the picker".dim(),
    ]));
    lines
}

fn short_thread_id(thread_id: &str) -> String {
    thread_id.chars().take(8).collect()
}

fn status_span(status: &PanelAgentStatus) -> Span<'static> {
    match status {
        PanelAgentStatus::PendingInit => "starting".cyan(),
        PanelAgentStatus::Running => "running".cyan().bold(),
        PanelAgentStatus::Interrupted => "interrupted".magenta(),
        PanelAgentStatus::Completed(_) => "completed".green(),
        PanelAgentStatus::Errored(_) => "errored".red(),
        PanelAgentStatus::Shutdown => "closed".dim(),
        PanelAgentStatus::NotFound => "not found".red(),
    }
}
