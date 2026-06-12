//! Status bar at the bottom of the screen.
//!
//! Displays:
//! - Session title (left)
//! - Model name (center)
//! - Token counts (right)
//! - Keyboard shortcuts hint

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::AppState;
use crate::theme::Theme;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let bg_style = Style::default().bg(Theme::STATUS_BG);

    // ── Left: session title ──
    let title_text = state
        .active_session
        .as_ref()
        .map(|s| s.title.clone())
        .unwrap_or_else(|| "No session".to_string());
    let title = Span::styled(
        format!("  {} ", title_text),
        Style::default().fg(Theme::TEXT).add_modifier(Modifier::BOLD),
    );

    // ── Center: model name ──
    let model = Span::styled(
        format!(" {} ", state.model_name),
        Style::default()
            .fg(Theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    );

    // ── Right: token counts ──
    let tokens = if state.total_input_tokens > 0 || state.total_output_tokens > 0 {
        format!(
            " {:.1}K / {:.1}K in/out ",
            state.total_input_tokens as f64 / 1000.0,
            state.total_output_tokens as f64 / 1000.0,
        )
    } else {
        String::new()
    };
    let token_span = Span::styled(
        tokens,
        Style::default().fg(Theme::TEXT_DIM),
    );

    // ── Help: keyboard shortcuts ──
    let shortcuts = Span::styled(
        " ^N new  ^S settings  ^L sidebar  Esc quit ",
        Style::default()
            .fg(Theme::TEXT_MUTED)
            .add_modifier(Modifier::DIM),
    );

    // Build layout with fill spacers.
    let line = Line::from(vec![title, model, token_span, shortcuts]);

    let paragraph = Paragraph::new(line).style(bg_style);

    // Fill entire status bar with bg color.
    let filled = ratatui::widgets::Block::new().style(bg_style);
    frame.render_widget(filled, area);
    frame.render_widget(paragraph, area);
}
