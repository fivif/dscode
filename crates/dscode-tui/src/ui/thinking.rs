//! Collapsible thinking block renderer.
//!
//! Thinking content (DeepSeek reasoning) is displayed in a dimmed,
//! italicized block with a toggle handle. By default, thinking blocks
//! are collapsed, showing only the header "Thinking (step N) [+/-]".
//! When expanded, the reasoning text is rendered in dim style.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// Render a thinking block as a sequence of `Line` values appended to `lines`.
pub fn render_thinking_lines<'a>(
    lines: &mut Vec<Line<'a>>,
    _idx: usize,
    step: &u32,
    content: &str,
    collapsed: bool,
) {
    let toggle = if collapsed { "[+]" } else { "[-]" };

    // ── Header ──
    lines.push(Line::from(vec![
        Span::styled(
            format!("  💭 Thinking (step {}) {} ", step, toggle),
            Style::default()
                .fg(Theme::THINK_FG)
                .add_modifier(Modifier::DIM)
                .add_modifier(Modifier::ITALIC),
        ),
    ]));

    // ── Body (when expanded) ──
    if !collapsed {
        for line_str in content.lines() {
            lines.push(Line::from(Span::styled(
                format!("    {}", line_str),
                Style::default()
                    .fg(Theme::THINK_FG)
                    .add_modifier(Modifier::DIM)
                    .add_modifier(Modifier::ITALIC),
            )));
        }
        lines.push(Line::from(""));
    }

    // Thin divider.
    lines.push(Line::from(Span::styled(
        "  ── thinking ──",
        Style::default()
            .fg(Theme::TEXT_MUTED)
            .add_modifier(Modifier::DIM),
    )));
    lines.push(Line::from(""));
}
