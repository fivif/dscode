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

/// Maximum rendered width of one result line, in chars (the `…` is included).
const MAX_LINE_CHARS: usize = 120;

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
                    // Truncate on a char boundary — byte slicing panics on CJK
                    // and emoji output. The ellipsis counts toward the 120 width.
                    let char_count = line_str.chars().count();
                    let truncated = if char_count > MAX_LINE_CHARS {
                        let kept: String = line_str.chars().take(MAX_LINE_CHARS - 1).collect();
                        format!("{}…", kept)
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
