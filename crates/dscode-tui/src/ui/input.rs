//! Multi-line input bar at the bottom of the screen.
//!
//! Features:
//! - Single-line display with scrolling for long input.
//! - Cursor position indicator.
//! - Model selector label on the left side.
//! - Input history accessible via Up/Down arrow keys.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::AppState;
use crate::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(Theme::BORDER))
        .bg(Theme::INPUT_BG);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // ── Model selector indicator (left side) ──
    let model_label = format!(" {} ", state.model_name);
    let _model_span = Span::styled(
        model_label,
        Style::default()
            .bg(Theme::BG_LIGHT)
            .fg(Theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    );

    // ── Input text with cursor ──
    let cursor_pos = state.input_cursor;
    let input_text = &state.input_buffer;

    // Build the visible input line (with cursor highlight).
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled(" ", Style::default()));

    if input_text.is_empty() && !state.is_streaming {
        spans.push(Span::styled(
            "Type a message... (Ctrl+N new chat, Esc to quit)",
            Style::default()
                .fg(Theme::TEXT_MUTED)
                .add_modifier(Modifier::DIM),
        ));
    } else if state.is_streaming {
        spans.push(Span::styled(
            "[Waiting for agent response...]",
            Style::default()
                .fg(Theme::WARNING)
                .add_modifier(Modifier::SLOW_BLINK),
        ));
    } else {
        // Show last ~60 characters for long inputs, keeping cursor in view.
        let max_visible = inner.width.saturating_sub(4) as usize; // space for model label + margins
        let max_visible = max_visible.max(10); // at least 10 chars

        let (start, end) = visible_range(input_text, cursor_pos, max_visible);

        for (i, ch) in input_text.chars().enumerate() {
            if i < start || i >= end {
                continue;
            }
            let ch_str = if ch == '\n' { "↵".to_string() } else { ch.to_string() };
            if i == cursor_pos {
                spans.push(Span::styled(
                    ch_str,
                    Style::default()
                        .bg(Theme::ACCENT)
                        .fg(Theme::BG_DARK)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(
                    ch_str,
                    Style::default().fg(Theme::TEXT),
                ));
            }
        }

        // Show cursor at end if past text.
        if cursor_pos >= end {
            spans.push(Span::styled(
                " ",
                Style::default()
                    .bg(Theme::ACCENT)
                    .fg(Theme::BG_DARK),
            ));
        }
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);

    frame.render_widget(paragraph, inner);

    // ── Render model label in the top-left of the input block ──
    // (We do a trick: render an overlapping block with the label.)
    // The model label is placed in the input area's top-left corner manually.
    let model_rect = Rect {
        x: area.x + 1,
        y: area.y,
        width: (state.model_name.len() + 2) as u16,
        height: 1,
    };
    let model_paragraph = Paragraph::new(Span::styled(
        format!(" {} ", state.model_name),
        Style::default()
            .bg(Theme::BG_LIGHT)
            .fg(Theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(model_paragraph, model_rect);
}

/// Compute the visible range of characters so the cursor stays in view,
/// centering it within the available width.
fn visible_range(text: &str, cursor: usize, max_width: usize) -> (usize, usize) {
    let char_len = text.chars().count();
    if char_len <= max_width {
        return (0, char_len);
    }

    // Try to center the cursor.
    let mut start = if cursor > max_width / 2 {
        cursor.saturating_sub(max_width / 2)
    } else {
        0
    };
    let end = (start + max_width).min(char_len);

    // Don't show more than exists.
    if end < char_len && end - start < max_width {
        start = start.saturating_sub(max_width - (end - start));
    }

    (start, end)
}
