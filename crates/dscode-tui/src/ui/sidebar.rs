//! Session list sidebar (left 30% panel).
//!
//! Displays sessions grouped by recency:
//! - Today
//! - Yesterday
//! - This Week
//! - This Month
//! - Older
//!
//! Sessions can be navigated with Shift+Up/Down and selected with Enter.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph};

use crate::app::AppState;
use crate::theme::Theme;

/// Maximum rendered title width, in chars (the `…` is included).
const MAX_TITLE_CHARS: usize = 30;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(Theme::BORDER))
        .bg(Theme::SIDEBAR_BG);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let entries = build_session_list(state);

    if entries.is_empty() {
        let hint = Paragraph::new(
            Line::from(Span::styled(
                "No sessions yet.\nCtrl+N to create one.",
                Style::default().fg(Theme::TEXT_DIM),
            )),
        )
        .centered();
        frame.render_widget(hint, inner);
        return;
    }

    let list = List::new(entries)
        .highlight_style(
            Style::default()
                .bg(Theme::BG_HIGHLIGHT)
                .fg(Theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        );

    // Scroll the list so the selected item is visible.
    let visible_height = inner.height as usize;
    let offset = state.session_select_index.map(|i| {
        if i < state.sidebar_scroll_offset {
            i
        } else if i >= state.sidebar_scroll_offset + visible_height {
            i - visible_height + 1
        } else {
            state.sidebar_scroll_offset
        }
    }).unwrap_or(0);

    frame.render_stateful_widget(
        list,
        inner,
        &mut ratatui::widgets::ListState::default()
            .with_selected(state.session_select_index)
            .with_offset(offset),
    );
}

/// Build the list items for the session sidebar.
fn build_session_list(state: &AppState) -> Vec<ListItem<'static>> {
    let mut items: Vec<ListItem> = Vec::new();

    let groups = [
        ("Today", &state.sessions_grouped.today),
        ("Yesterday", &state.sessions_grouped.yesterday),
        ("This Week", &state.sessions_grouped.this_week),
        ("This Month", &state.sessions_grouped.this_month),
        ("Older", &state.sessions_grouped.older),
    ];

    for (group_name, sessions) in &groups {
        if sessions.is_empty() {
            continue;
        }

        // Group header.
        items.push(ListItem::new(Span::styled(
            format!(" ─ {} ─", group_name),
            Style::default()
                .fg(Theme::TEXT_DIM)
                .add_modifier(Modifier::BOLD),
        )));

        for session in *sessions {
            // Truncate on a char boundary: byte slicing panics on CJK titles
            // (10 Chinese chars = 30 bytes). `…` counts toward the 30 width.
            let title = if session.title.chars().count() > MAX_TITLE_CHARS {
                let kept: String = session.title.chars().take(MAX_TITLE_CHARS - 1).collect();
                format!("{}…", kept)
            } else {
                session.title.clone()
            };

            let is_active = state
                .active_session
                .as_ref()
                .map(|s| s.id == session.id)
                .unwrap_or(false);

            let style = if is_active {
                Style::default().fg(Theme::ACCENT)
            } else {
                Style::default().fg(Theme::TEXT)
            };

            let line = Line::from(vec![
                Span::styled(
                    if is_active { "▸ " } else { "  " },
                    style,
                ),
                Span::styled(title, style),
            ]);

            items.push(ListItem::new(line));
        }
    }

    items
}
