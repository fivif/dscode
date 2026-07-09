//! Chat message view (right 70% panel).
//!
//! Renders the message history as a scrollable list. Each message type
//! (User, Assistant, Thinking, ToolCard, Fact, Error, Completion) has
//! its own visual treatment. Streaming content (during forge execution)
//! is appended in-place.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{AppState, UiMessage};
use crate::theme::Theme;
use crate::ui::cards;
use crate::ui::thinking;

pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .borders(Borders::NONE)
        .bg(Theme::BG);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.messages.is_empty() {
        let hint_text = if state.active_session.is_none() {
            "No session selected.\nUse Ctrl+N to create a new chat or select one from the sidebar."
        } else {
            "Start a conversation — type a message and press Enter."
        };
        let hint = Paragraph::new(hint_text)
            .style(Style::default().fg(Theme::TEXT_DIM))
            .centered();
        frame.render_widget(hint, inner);
        return;
    }

    // Build message lines, handling scroll.
    let lines = build_message_lines(state);
    let visible_height = inner.height as usize;

    let start = state.chat_scroll_offset.min(lines.len().saturating_sub(1));
    let end = (start + visible_height).min(lines.len());
    let visible_lines: Vec<Line> = lines[start..end].to_vec();

    let text = Text::from(visible_lines);
    let paragraph = Paragraph::new(text)
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, inner);
}

/// Build all message lines for display. Each message expands to one or more
/// `Line` values. Tool cards and thinking blocks are rendered inline here.
fn build_message_lines<'a>(state: &'a AppState) -> Vec<Line<'a>> {
    let mut lines: Vec<Line> = Vec::new();
    let mut thinking_idx = 0usize;
    let mut card_idx = 0usize;

    for msg in &state.messages {
        match msg {
            UiMessage::User { content, .. } => {
                lines.push(Line::from(Span::styled(
                    format!("▸ You: {}", content),
                    Style::default()
                        .fg(Theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
            }
            UiMessage::Assistant { content, .. } => {
                // Render markdown-like content.
                for paragraph in content.split("\n\n") {
                    for line in paragraph.lines() {
                        let styled = if line.starts_with("```") {
                            Span::styled(
                                line,
                                Style::default().fg(Theme::TEXT_MUTED),
                            )
                        } else if line.starts_with("# ") || line.starts_with("## ") {
                            Span::styled(
                                line,
                                Style::default()
                                    .fg(Theme::ACCENT)
                                    .add_modifier(Modifier::BOLD),
                            )
                        } else if line.starts_with("- ") || line.starts_with("* ") {
                            Span::styled(
                                format!("  {}", line),
                                Style::default()
                                    .fg(Theme::TEXT)
                                    .add_modifier(Modifier::DIM),
                            )
                        } else if line.starts_with("> ") {
                            Span::styled(
                                line,
                                Style::default().fg(Theme::TEXT_DIM),
                            )
                        } else {
                            Span::styled(
                                line,
                                Style::default().fg(Theme::TEXT),
                            )
                        };
                        lines.push(Line::from(styled));
                    }
                    lines.push(Line::from(""));
                }
            }
            UiMessage::Thinking {
                content,
                step,
                collapsed,
            } => {
                let idx = thinking_idx;
                thinking_idx += 1;
                thinking::render_thinking_lines(
                    &mut lines,
                    idx,
                    step,
                    content,
                    *collapsed,
                );
            }
            UiMessage::ToolCard {
                id,
                name,
                description,
                result,
                status,
                collapsed,
            } => {
                let idx = card_idx;
                card_idx += 1;
                cards::render_tool_card_lines(
                    &mut lines,
                    idx,
                    id,
                    name,
                    description,
                    result.as_deref(),
                    status,
                    *collapsed,
                );
            }
            UiMessage::Fact {
                subject,
                predicate,
                object,
            } => {
                lines.push(Line::from(vec![
                    Span::styled("  🧠 ", Style::default().fg(Theme::INFO)),
                    Span::styled(
                        format!("{} {} {}", subject, predicate, object),
                        Style::default()
                            .fg(Theme::TEXT_DIM)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]));
                lines.push(Line::from(""));
            }
            UiMessage::Error { content } => {
                lines.push(Line::from(Span::styled(
                    format!("  ❌ {}", content),
                    Style::default()
                        .fg(Theme::ERROR)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
            }
            UiMessage::Completion { usage } => {
                let usage_text = if let Some(ref u) = usage {
                    format!(
                        " ── ✓ Complete · {:.1}K tokens (in {:.1}K, out {:.1}K) ──",
                        (u.input_tokens + u.output_tokens) as f64 / 1000.0,
                        u.input_tokens as f64 / 1000.0,
                        u.output_tokens as f64 / 1000.0,
                    )
                } else {
                    " ── ✓ Complete ──".to_string()
                };
                lines.push(Line::from(Span::styled(
                    usage_text,
                    Style::default()
                        .fg(Theme::SUCCESS)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
            }
        }
    }

    // Streaming indicator.
    if state.is_streaming {
        lines.push(Line::from(Span::styled(
            " ⏳ Streaming…",
            Style::default()
                .fg(Theme::ACCENT)
                .add_modifier(Modifier::SLOW_BLINK),
        )));
    }

    lines
}
