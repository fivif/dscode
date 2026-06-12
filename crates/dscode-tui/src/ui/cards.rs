//! Tool call cards — expandable blocks showing tool execution details.
//!
//! Each tool call is rendered as a bordered card:
//! - Header row: icon (🔧/✅/❌), tool name, description, toggle hint.
//! - Body: the tool's output/result when expanded.
//! - Auto-collapses to header-only on completion.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::app::ToolCardStatus;
use crate::theme::Theme;

/// Render a tool card as a sequence of `Line` values appended to `lines`.
pub fn render_tool_card_lines<'a>(
    lines: &mut Vec<Line<'a>>,
    _idx: usize,
    _id: &str,
    name: &str,
    description: &str,
    result: Option<&str>,
    status: &ToolCardStatus,
    collapsed: bool,
) {
    let (icon, icon_color) = match status {
        ToolCardStatus::Running => ("🔧", Theme::WARNING),
        ToolCardStatus::Success => ("✅", Theme::SUCCESS),
        ToolCardStatus::Error => ("❌", Theme::ERROR),
    };

    // ── Header ──
    let toggle_hint = if *status != ToolCardStatus::Running {
        if collapsed { "[+]" } else { "[-]" }
    } else {
        ""
    };

    let header_line = Line::from(vec![
        Span::styled(
            format!("{} ", icon),
            Style::default().fg(icon_color),
        ),
        Span::styled(
            format!("{} ", name),
            Style::default()
                .fg(Theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            description.to_string(),
            Style::default()
                .fg(Theme::TEXT_DIM)
                .add_modifier(Modifier::DIM),
        ),
        Span::styled(
            format!(" {}", toggle_hint),
            Style::default()
                .fg(Theme::TEXT_MUTED)
                .add_modifier(Modifier::DIM),
        ),
    ]);

    lines.push(header_line);

    // ── Body (when expanded) ──
    if !collapsed {
        if let Some(output) = result {
            if !output.is_empty() {
                for line_str in output.lines() {
                    let truncated = if line_str.len() > 120 {
                        format!("{}…", &line_str[..119])
                    } else {
                        line_str.to_string()
                    };
                    lines.push(Line::from(Span::styled(
                        format!("  │ {}", truncated),
                        Style::default().fg(Theme::TEXT_DIM),
                    )));
                }
            }
        }

        if matches!(status, ToolCardStatus::Running) {
            lines.push(Line::from(Span::styled(
                "  │ ⏳ Running…",
                Style::default()
                    .fg(Theme::WARNING)
                    .add_modifier(Modifier::SLOW_BLINK),
            )));
        }
    }

    // ── Card border bottom ──
    lines.push(Line::from(Span::styled(
        "  ──────────────────────────────────",
        Style::default().fg(Theme::BORDER),
    )));
    lines.push(Line::from(""));
}
