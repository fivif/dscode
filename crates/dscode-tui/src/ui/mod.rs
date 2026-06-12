//! UI rendering for the DS Code TUI.
//!
//! The layout is:
//!
//! ```text
//! ┌──────────┬─────────────────────────────────────┐
//! │          │                                     │
//! │ Sidebar  │           Chat View                 │
//! │  (30%)   │           (70%)                     │
//! │          │                                     │
//! │          │                                     │
//! ├──────────┴─────────────────────────────────────┤
//! │                Input Bar                       │
//! ├────────────────────────────────────────────────┤
//! │                Status Bar                      │
//! └────────────────────────────────────────────────┘
//! ```

pub mod sidebar;
pub mod chat;
pub mod input;
pub mod cards;
pub mod thinking;
pub mod status;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};

use crate::app::AppState;
use crate::theme::Theme;

/// Render the entire TUI frame.
pub fn render(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

    // Prevent degenerate frames during startup/resize.
    if area.width < 20 || area.height < 8 {
        return;
    }

    let _bg = ratatui::widgets::Block::new()
        .style(ratatui::style::Style::default().bg(Theme::BG));

    // ── Split vertically: main content, input bar, status bar ──
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),  // chat + sidebar
            Constraint::Length(3), // input bar
            Constraint::Length(1), // status bar
        ])
        .split(area);

    let main_area = v_chunks[0];
    let input_area = v_chunks[1];
    let status_area = v_chunks[2];

    // ── Split main horizontally: sidebar (30%) | chat (70%) ──
    let h_chunks = if state.sidebar_visible {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(30),
                Constraint::Percentage(70),
            ])
            .split(main_area)
    } else {
        // Sidebar hidden — chat takes 100%.
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(0),
                Constraint::Percentage(100),
            ])
            .split(main_area)
    };

    let sidebar_area = h_chunks[0];
    let chat_area = h_chunks[1];

    // ── Render each panel ──
    if state.sidebar_visible {
        sidebar::render(frame, sidebar_area, state);
    }
    chat::render(frame, chat_area, state);
    input::render(frame, input_area, state);
    status::render(frame, status_area, state);
}
