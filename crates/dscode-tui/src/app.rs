//! Application state and main event loop.
//!
//! `AppState` holds the entire TUI model: sessions, messages, input,
//! streaming state, and UI flags. The `run` function is the top-level
//! event loop that reads user input, spawns Forge tasks, receives
//! `StreamEvent` values, and renders the UI at each tick.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use crossterm::event::{self, Event as CrosstermEvent, KeyCode};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use dscode_core::agent::forge::Forge;
use dscode_core::agent::stream::{StreamEvent, ToolStatus, UsageInfo};
use dscode_core::config::settings::Config;
use dscode_core::providers::create_provider;
use dscode_core::providers::trait_def::{Message, MessageContent, Role};
use dscode_core::safety::{PermissionHub, SafetyGuard};
use dscode_core::session::manager::{Session, SessionGroups, SessionManager};
use dscode_core::tools::registry::ToolRegistry;

use crate::events::{key_event_to_action, mouse_event_to_action, Action, KeyContext};
use crate::ui;

/// A single rendered message in the chat view.
#[derive(Debug, Clone)]
pub enum UiMessage {
    /// A user message.
    User { content: String, timestamp: i64 },
    /// An assistant text token (accumulated streaming text).
    Assistant { content: String, timestamp: i64 },
    /// A thinking block (collapsible).
    Thinking {
        content: String,
        step: u32,
        collapsed: bool,
    },
    /// A tool call card (expandable).
    ToolCard {
        id: String,
        name: String,
        description: String,
        result: Option<String>,
        status: ToolCardStatus,
        collapsed: bool,
    },
    /// A fact extracted by the model.
    Fact {
        subject: String,
        predicate: String,
        object: String,
    },
    /// An error message.
    Error { content: String },
    /// Completion marker with usage info.
    Completion { usage: Option<UsageInfo> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolCardStatus {
    Running,
    Success,
    Error,
}

/// Selectable item index type.
pub type SelectIndex = Option<usize>;

/// A tool-confirmation request raised by the safety layer that is waiting for
/// the user to answer y/n.
#[derive(Debug, Clone)]
pub struct PendingPermission {
    /// Request id used to answer the [`PermissionHub`].
    pub id: String,
    /// The command that needs confirmation.
    pub command: String,
}

/// The complete application state.
pub struct AppState {
    // ── Session ──
    pub session_manager: SessionManager,
    pub sessions_grouped: SessionGroups,
    pub active_session: Option<Session>,
    pub session_select_index: SelectIndex,
    pub new_session_title: String,

    // ── Chat ──
    pub messages: Vec<UiMessage>,
    /// Scroll offset measured **from the bottom** of the transcript:
    /// 0 = pinned to the newest line, N = N lines scrolled up.
    pub chat_scroll_offset: usize,
    /// Index of the first message produced by the current turn. Everything
    /// before it has already been persisted.
    pub turn_start: usize,

    // ── Sidebar ──
    pub sidebar_scroll_offset: usize,

    // ── Input ──
    pub input_buffer: String,
    pub input_cursor: usize,
    pub input_history: Vec<String>,
    pub history_index: usize,

    // ── Streaming state ──
    pub is_streaming: bool,
    pub streaming_accumulator: String,

    // ── Model ──
    pub model_name: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,

    // ── Config ──
    pub config: Config,
    pub working_dir: PathBuf,
    pub tool_registry: Arc<ToolRegistry>,

    // ── Permissions ──
    /// Shared permission gate. The Forge emits `PermissionRequest` through the
    /// stream channel; the user answers it in the input bar with y/n.
    pub permission_hub: Arc<PermissionHub>,
    /// Confirmation requests waiting for a y/n answer, in arrival order
    /// (parallel tool calls can raise more than one).
    pub pending_permissions: VecDeque<PendingPermission>,

    // ── UI flags ──
    pub sidebar_visible: bool,
    pub show_settings: bool,
    pub should_quit: bool,
}

impl AppState {
    /// Create a new AppState with loaded configuration, session manager,
    /// and tool registry.
    pub fn new() -> Result<Self> {
        let config = Config::load()?;
        let working_dir = std::env::current_dir()?;
        let session_manager = SessionManager::new(config.session.retention_days)
            .map_err(|e| anyhow::anyhow!("Failed to open session database: {}", e))?;
        let sessions_grouped = session_manager
            .get_sessions_grouped()
            .unwrap_or(SessionGroups {
                today: vec![],
                yesterday: vec![],
                this_week: vec![],
                this_month: vec![],
                older: vec![],
            });

        let task_manager = dscode_core::tools::background::TaskManager::new();
        let handle = task_manager.handle();
        let notify_tx = task_manager.notify_tx();
        let mut tool_registry = ToolRegistry::new();
        // Same as the server: `image_enabled = false` means the tool is not
        // registered, not merely that it refuses to run.
        tool_registry.register_default_tools_with_image(config.generation.image_enabled);
        tool_registry.register(dscode_core::tools::background::DoBackground::new(handle.clone(), task_manager.live_handle(), notify_tx));
        tool_registry.register(dscode_core::tools::background::DoTaskStatus::new(handle));
        let tool_registry = Arc::new(tool_registry);

        Ok(Self {
            session_manager,
            sessions_grouped,
            active_session: None,
            session_select_index: None,
            new_session_title: String::new(),
            messages: vec![],
            chat_scroll_offset: 0,
            turn_start: 0,
            sidebar_scroll_offset: 0,
            input_buffer: String::new(),
            input_cursor: 0,
            input_history: vec![],
            history_index: 0,
            is_streaming: false,
            streaming_accumulator: String::new(),
            model_name: config.default_model.clone(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            config,
            working_dir,
            tool_registry,
            permission_hub: PermissionHub::shared(),
            pending_permissions: VecDeque::new(),
            sidebar_visible: true,
            show_settings: false,
            should_quit: false,
        })
    }

    /// Refresh the session list from the database.
    pub fn refresh_sessions(&mut self) {
        if let Ok(groups) = self.session_manager.get_sessions_grouped() {
            self.sessions_grouped = groups;
        }
    }

    /// Load a session by ID, populating the message view.
    pub fn load_session(&mut self, session_id: &str) {
        match self.session_manager.get_session(session_id) {
            Ok(Some(session)) => {
                // Convert stored messages to UiMessages for display. Real
                // timestamps and reasoning content are preserved so a reopened
                // session looks like it did when it was live.
                let mut messages: Vec<UiMessage> = Vec::with_capacity(session.messages.len());
                let mut thinking_step = 0u32;
                for msg in &session.messages {
                    let ts = msg.created_at;
                    let text = msg.content.as_text().unwrap_or("").to_string();
                    match msg.role {
                        Role::User => messages.push(UiMessage::User {
                            content: text,
                            timestamp: ts,
                        }),
                        Role::Assistant => {
                            // Reasoning is stored on its own row (empty content);
                            // render it as a collapsible thinking block.
                            if let Some(reasoning) = msg
                                .reasoning_content
                                .as_deref()
                                .filter(|r| !r.trim().is_empty())
                            {
                                messages.push(UiMessage::Thinking {
                                    content: reasoning.to_string(),
                                    step: thinking_step,
                                    collapsed: true,
                                });
                                thinking_step += 1;
                            }
                            if !text.is_empty() {
                                messages.push(UiMessage::Assistant {
                                    content: text,
                                    timestamp: ts,
                                });
                            }
                        }
                        Role::Tool => {
                            let description =
                                text.lines().next().unwrap_or("").trim().to_string();
                            messages.push(UiMessage::ToolCard {
                                id: msg.tool_call_id.clone().unwrap_or_default(),
                                name: msg.name.clone().unwrap_or_default(),
                                description,
                                result: Some(text),
                                status: ToolCardStatus::Success,
                                collapsed: true,
                            });
                        }
                        Role::System => messages.push(UiMessage::Assistant {
                            content: format!("[system] {}", text),
                            timestamp: ts,
                        }),
                    }
                }
                self.messages = messages;
                self.active_session = Some(session);
                // Offset is measured from the bottom, so 0 shows the newest lines.
                self.chat_scroll_offset = 0;
            }
            Ok(None) => {
                self.messages = vec![];
                self.active_session = None;
            }
            Err(_) => {}
        }
    }

    /// Create a new session and switch to it.
    pub fn create_session(&mut self, title: &str) {
        let title = if title.is_empty() { "New Chat" } else { title };
        let ws = self.working_dir.to_string_lossy().to_string();
        match self.session_manager.create_session(title, &ws, &self.config.default_model) {
            Ok(session) => {
                let id = session.id.clone();
                self.active_session = Some(session);
                self.messages = vec![];
                self.chat_scroll_offset = 0;
                self.refresh_sessions();
                // Find and select the new session in the sidebar.
                self.select_session_in_sidebar(&id);
            }
            Err(_) => {}
        }
    }

    /// Delete the currently selected session from the sidebar.
    pub fn delete_selected_session(&mut self) {
        if let Some(session) = self.selected_session() {
            let id = session.id.clone();
            if self.session_manager.delete_session(&id).is_ok() {
                if let Some(ref active) = self.active_session {
                    if active.id == id {
                        self.active_session = None;
                        self.messages = vec![];
                    }
                }
                self.refresh_sessions();
                self.session_select_index = None;
            }
        }
    }

    /// Get the currently selected session from the sidebar.
    fn selected_session(&self) -> Option<&Session> {
        self.session_select_index.and_then(|idx| {
            let all_sessions = self.all_sessions_flat();
            all_sessions.get(idx).copied()
        })
    }

    /// Flat list of all sessions matching the grouped display order.
    pub fn all_sessions_flat(&self) -> Vec<&Session> {
        let mut vec = Vec::new();
        vec.extend(&self.sessions_grouped.today);
        vec.extend(&self.sessions_grouped.yesterday);
        vec.extend(&self.sessions_grouped.this_week);
        vec.extend(&self.sessions_grouped.this_month);
        vec.extend(&self.sessions_grouped.older);
        vec
    }

    /// Set the sidebar selection to the given session ID if found.
    fn select_session_in_sidebar(&mut self, id: &str) {
        let all = self.all_sessions_flat();
        self.session_select_index = all.iter().position(|s| s.id == id);
    }

    /// Byte offset of a char index in `input_buffer`, clamped to the end.
    ///
    /// `input_cursor` is a **char** index (the renderer and every mutation use
    /// it that way); `String::insert`/`remove` need a byte index.
    fn input_byte_offset(&self, char_idx: usize) -> usize {
        self.input_buffer
            .char_indices()
            .nth(char_idx)
            .map(|(byte, _)| byte)
            .unwrap_or(self.input_buffer.len())
    }

    /// Number of chars in the input buffer.
    fn input_char_len(&self) -> usize {
        self.input_buffer.chars().count()
    }

    /// Submit the current input as a user message and launch the Forge.
    pub fn submit_input(&mut self) {
        let message = std::mem::take(&mut self.input_buffer);
        self.input_cursor = 0;

        if message.trim().is_empty() {
            return;
        }

        // Save to history.
        self.input_history.push(message.clone());
        self.history_index = self.input_history.len();

        // Ensure active session exists.
        if self.active_session.is_none() {
            self.create_session("New Chat");
        }

        let Some(ref mut session) = self.active_session else {
            return;
        };

        let now = Utc::now().timestamp();

        // Everything from here on belongs to this turn — the persistence step on
        // `Complete` only writes messages at or after this index.
        self.turn_start = self.messages.len();

        // Append user message to UI.
        self.messages.push(UiMessage::User {
            content: message.clone(),
            timestamp: now,
        });

        // Persist user message.
        let _ = self.session_manager.add_message(
            &session.id,
            &Message {
                role: Role::User,
                content: MessageContent::Text(message.clone()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
                created_at: 0,
            },
        );

        self.is_streaming = true;
        self.streaming_accumulator = String::new();
        // Pin the view to the bottom so the answer is visible as it streams.
        self.chat_scroll_offset = 0;
    }

    /// Handle a StreamEvent from the running forge.
    pub fn handle_stream_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Thinking { content, step } => {
                self.messages.push(UiMessage::Thinking {
                    content,
                    step,
                    collapsed: true,
                });
            }
            StreamEvent::Token { content } => {
                // Tokens arrive sequentially for streaming.
                // If the last message is already an Assistant, append.
                // Otherwise, start a new Assistant message.
                self.streaming_accumulator.push_str(&content);

                let last_is_assistant = matches!(
                    self.messages.last(),
                    Some(UiMessage::Assistant { .. })
                );
                if last_is_assistant {
                    // Replace last with updated accumulator.
                    if let Some(UiMessage::Assistant { .. }) = self.messages.last_mut() {
                        *self.messages.last_mut().unwrap() = UiMessage::Assistant {
                            content: self.streaming_accumulator.clone(),
                            timestamp: Utc::now().timestamp(),
                        };
                    }
                } else {
                    self.messages.push(UiMessage::Assistant {
                        content: content.clone(),
                        timestamp: Utc::now().timestamp(),
                    });
                }
            }
            StreamEvent::ToolStart {
                id,
                name,
                description,
                arguments: _,
            } => {
                self.messages.push(UiMessage::ToolCard {
                    id,
                    name,
                    description,
                    result: None,
                    status: ToolCardStatus::Running,
                    collapsed: false,
                });
            }
            StreamEvent::ToolProgress { id, chunk } => {
                // Append chunk to the running tool card result.
                for msg in self.messages.iter_mut().rev() {
                    if let UiMessage::ToolCard {
                        id: ref tool_id,
                        ref mut result,
                        ..
                    } = msg
                    {
                        if *tool_id == id {
                            if let Some(ref mut r) = result {
                                r.push_str(&chunk);
                            } else {
                                *result = Some(chunk);
                            }
                            break;
                        }
                    }
                }
            }
            StreamEvent::ToolEnd { id, status, result } => {
                let card_status = match status {
                    ToolStatus::Success => ToolCardStatus::Success,
                    ToolStatus::Error => ToolCardStatus::Error,
                    ToolStatus::Running => ToolCardStatus::Running,
                };
                let result_str = result;
                for msg in self.messages.iter_mut().rev() {
                    if let UiMessage::ToolCard {
                        id: ref tool_id,
                        ref mut status,
                        ref mut result,
                        ref mut collapsed,
                        ..
                    } = msg
                    {
                        if *tool_id == id {
                            *status = card_status;
                            *result = Some(result_str.clone());
                            *collapsed = true; // Auto-collapse on completion.
                            break;
                        }
                    }
                }
            }
            StreamEvent::Fact {
                subject,
                predicate,
                object,
                ..
            } => {
                self.messages.push(UiMessage::Fact {
                    subject,
                    predicate,
                    object,
                });
            }
            StreamEvent::ContextCompressed {
                before_tokens,
                after_tokens,
                window: _,
            } => {
                self.messages.push(UiMessage::Assistant {
                    content: format!(
                        "⚠️ 上下文已自动压缩：{before_tokens} → {after_tokens} tokens"
                    ),
                    timestamp: Utc::now().timestamp(),
                });
            }
            StreamEvent::TeamAgentStart { agent_id, task } => {
                self.messages.push(UiMessage::Assistant {
                    content: format!("▶ Agent `{agent_id}` started: {task}"),
                    timestamp: Utc::now().timestamp(),
                });
            }
            StreamEvent::TeamAgentOutput { agent_id, content } => {
                self.streaming_accumulator
                    .push_str(&format!("[{agent_id}] {content}"));
                self.messages.push(UiMessage::Assistant {
                    content: format!("[{agent_id}] {content}"),
                    timestamp: Utc::now().timestamp(),
                });
            }
            StreamEvent::TeamAgentEnd {
                agent_id,
                success,
                summary,
            } => {
                let mark = if success { "✓" } else { "✗" };
                self.messages.push(UiMessage::Assistant {
                    content: format!("{mark} Agent `{agent_id}` finished: {summary}"),
                    timestamp: Utc::now().timestamp(),
                });
            }
            StreamEvent::TeamComplete { completed, failed } => {
                self.messages.push(UiMessage::Assistant {
                    content: format!("Team complete — {completed} ok, {failed} failed"),
                    timestamp: Utc::now().timestamp(),
                });
            }
            StreamEvent::PlanQuestion {
                phase,
                question,
                recommended,
                options,
                ..
            } => {
                let mut text = format!(
                    "## Plan: {} — {}\n\n推荐: {}\n",
                    phase, question, recommended
                );
                for (i, opt) in options.iter().enumerate() {
                    text.push_str(&format!("{}. {}\n", i + 1, opt));
                }
                self.messages.push(UiMessage::Assistant {
                    content: text,
                    timestamp: Utc::now().timestamp(),
                });
            }
            StreamEvent::PermissionRequest {
                id,
                command,
                reason,
                timeout_secs,
                ..
            } => {
                self.pending_permissions.push_back(PendingPermission {
                    id,
                    command: command.clone(),
                });
                self.messages.push(UiMessage::Assistant {
                    content: format!(
                        "⚠️ Permission required ({timeout_secs}s):\n`{command}`\n{reason}\n\
                         Press y to approve, n to deny (Ctrl+Q to quit)."
                    ),
                    timestamp: Utc::now().timestamp(),
                });
            }
            StreamEvent::Error { content } => {
                self.messages.push(UiMessage::Error { content });
                // The forge emits `Error` on every fatal path and then returns
                // without a `Complete`; without this the loop would relaunch the
                // same request forever.
                self.is_streaming = false;
            }
            StreamEvent::Complete { usage } => {
                // Finalize the streaming accumulator as a settled assistant message.
                let assistant_text = std::mem::take(&mut self.streaming_accumulator);

                if let Some(ref u) = usage {
                    self.total_input_tokens += u.input_tokens;
                    self.total_output_tokens += u.output_tokens;
                }

                self.messages
                    .push(UiMessage::Completion { usage });

                // ── Persist assistant and tool messages to the session database ──
                if let Some(ref session) = self.active_session {
                    let session_id = &session.id;
                    let now = Utc::now().timestamp();

                    // Persist the assistant text response.
                    if !assistant_text.is_empty() {
                        let _ = self.session_manager.add_message(
                            session_id,
                            &Message {
                                role: Role::Assistant,
                                content: MessageContent::Text(assistant_text),
                                name: None,
                                tool_calls: None,
                                tool_call_id: None,
                                reasoning_content: None,
                                created_at: now,
                            },
                        );
                    }

                    // Persist each tool card as a Role::Tool message. Only this
                    // turn's messages are written — earlier turns were already
                    // persisted and re-writing them would grow the DB every turn.
                    let turn_start = self.turn_start.min(self.messages.len());
                    for msg in &self.messages[turn_start..] {
                        if let UiMessage::ToolCard {
                            id,
                            name,
                            result,
                            status,
                            ..
                        } = msg
                        {
                            if *status != ToolCardStatus::Running {
                                let _ = self.session_manager.add_message(
                                    session_id,
                                    &Message {
                                        role: Role::Tool,
                                        content: MessageContent::Text(
                                            result.clone().unwrap_or_default(),
                                        ),
                                        name: if name.is_empty() {
                                            None
                                        } else {
                                            Some(name.clone())
                                        },
                                        tool_calls: None,
                                        tool_call_id: if id.is_empty() {
                                            None
                                        } else {
                                            Some(id.clone())
                                        },
                                        reasoning_content: None,
                                        created_at: now,
                                    },
                                );
                            }
                        }
                    }

                    // Persist thinking blocks as reasoning content.
                    for msg in &self.messages[turn_start..] {
                        if let UiMessage::Thinking { content, .. } = msg {
                            let _ = self.session_manager.add_message(
                                session_id,
                                &Message {
                                    role: Role::Assistant,
                                    content: MessageContent::Text(String::new()),
                                    name: None,
                                    tool_calls: None,
                                    tool_call_id: None,
                                    reasoning_content: Some(content.clone()),
                                    created_at: now,
                                },
                            );
                        }
                    }
                }

                self.is_streaming = false;
            }
        }
    }

    /// Handle a user-triggered Action.
    pub fn handle_action(&mut self, action: Action) {
        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::NewChat => {
                let title = self.new_session_title.clone();
                self.create_session(&title);
            }
            Action::Settings => {
                self.show_settings = !self.show_settings;
            }
            Action::ToggleSidebar => {
                self.sidebar_visible = !self.sidebar_visible;
            }
            Action::Submit => {
                if !self.is_streaming {
                    self.submit_input();
                }
            }
            Action::Backspace => {
                if self.input_cursor > 0 {
                    // `input_cursor` is a char index — convert before removing,
                    // otherwise multi-byte chars (CJK, emoji) panic.
                    self.input_cursor -= 1;
                    let at = self.input_byte_offset(self.input_cursor);
                    self.input_buffer.remove(at);
                }
            }
            Action::Delete => {
                if self.input_cursor < self.input_char_len() {
                    let at = self.input_byte_offset(self.input_cursor);
                    self.input_buffer.remove(at);
                }
            }
            Action::CursorLeft => {
                self.input_cursor = self.input_cursor.saturating_sub(1);
            }
            Action::CursorRight => {
                if self.input_cursor < self.input_char_len() {
                    self.input_cursor += 1;
                }
            }
            Action::CursorHome => {
                self.input_cursor = 0;
            }
            Action::CursorEnd => {
                self.input_cursor = self.input_char_len();
            }
            Action::InsertChar(c) => {
                let at = self.input_byte_offset(self.input_cursor);
                self.input_buffer.insert(at, c);
                self.input_cursor += 1;
            }
            Action::InsertNewline => {
                let at = self.input_byte_offset(self.input_cursor);
                self.input_buffer.insert(at, '\n');
                self.input_cursor += 1;
            }
            Action::SessionUp => {
                if let Some(idx) = self.session_select_index {
                    if idx > 0 {
                        self.session_select_index = Some(idx - 1);
                    }
                } else if !self.all_sessions_flat().is_empty() {
                    self.session_select_index = Some(0);
                }
            }
            Action::SessionDown => {
                let all = self.all_sessions_flat();
                if all.is_empty() {
                    return;
                }
                match self.session_select_index {
                    Some(idx) if idx + 1 < all.len() => {
                        self.session_select_index = Some(idx + 1);
                    }
                    None => {
                        self.session_select_index = Some(0);
                    }
                    _ => {}
                }
            }
            Action::SessionSelect => {
                if let Some(session) = self.selected_session() {
                    // Avoid re-selecting the already active session.
                    if let Some(ref active) = self.active_session {
                        if active.id == session.id {
                            return;
                        }
                    }
                    let id = session.id.clone();
                    self.load_session(&id);
                }
            }
            Action::SessionDelete => {
                self.delete_selected_session();
            }
            Action::ScrollUp => {
                // Offset counts lines hidden below the viewport, so scrolling
                // up means increasing it.
                self.chat_scroll_offset = self.chat_scroll_offset.saturating_add(1);
            }
            Action::ScrollDown => {
                self.chat_scroll_offset = self.chat_scroll_offset.saturating_sub(1);
            }
            Action::ScrollPageUp => {
                self.chat_scroll_offset = self.chat_scroll_offset.saturating_add(10);
            }
            Action::ScrollPageDown => {
                self.chat_scroll_offset = self.chat_scroll_offset.saturating_sub(10);
            }
            Action::ScrollBottom => {
                // 0 == pinned to the newest line.
                self.chat_scroll_offset = 0;
            }
            Action::ToggleThinking(idx) => {
                let mut msg_idx = 0;
                for msg in self.messages.iter_mut() {
                    if let UiMessage::Thinking { .. } = msg {
                        if msg_idx == idx {
                            if let UiMessage::Thinking { collapsed, .. } = msg {
                                *collapsed = !*collapsed;
                            }
                            break;
                        }
                        msg_idx += 1;
                    }
                }
            }
            Action::ToggleLatestThinking => {
                let count = self
                    .messages
                    .iter()
                    .filter(|m| matches!(m, UiMessage::Thinking { .. }))
                    .count();
                if count > 0 {
                    self.handle_action(Action::ToggleThinking(count - 1));
                }
            }
            Action::ToggleToolCard(idx) => {
                let mut card_idx = 0;
                for msg in self.messages.iter_mut() {
                    if let UiMessage::ToolCard { .. } = msg {
                        if card_idx == idx {
                            if let UiMessage::ToolCard { collapsed, .. } = msg {
                                *collapsed = !*collapsed;
                            }
                            break;
                        }
                        card_idx += 1;
                    }
                }
            }
            Action::ToggleLatestToolCard => {
                let count = self
                    .messages
                    .iter()
                    .filter(|m| matches!(m, UiMessage::ToolCard { .. }))
                    .count();
                if count > 0 {
                    self.handle_action(Action::ToggleToolCard(count - 1));
                }
            }
            Action::HistoryPrevious => {
                if !self.input_history.is_empty() && self.history_index > 0 {
                    self.history_index -= 1;
                    self.input_buffer = self.input_history[self.history_index].clone();
                    self.input_cursor = self.input_char_len();
                }
            }
            Action::HistoryNext => {
                if self.history_index < self.input_history.len() {
                    self.history_index += 1;
                    if self.history_index < self.input_history.len() {
                        self.input_buffer = self.input_history[self.history_index].clone();
                    } else {
                        self.input_buffer.clear();
                    }
                    self.input_cursor = self.input_char_len();
                }
            }
            Action::Noop => {}
        }
    }
}

/// Shared application handle used by the event loop.
pub struct App {
    pub state: AppState,
    /// Handle to the currently running forge task, if any.
    pub forge_handle: Option<(String, JoinHandle<()>)>,
    /// Receiver for stream events from the running forge.
    pub event_rx: Option<mpsc::UnboundedReceiver<StreamEvent>>,
}

impl App {
    pub fn new() -> Result<Self> {
        let state = AppState::new()?;

        Ok(Self {
            state,
            forge_handle: None,
            event_rx: None,
        })
    }

    /// Answer the pending tool-confirmation request and record the decision in
    /// the transcript. No-op when nothing is pending.
    pub async fn resolve_pending_permission(&mut self, allow: bool) {
        let Some(pending) = self.state.pending_permissions.pop_front() else {
            return;
        };
        let result = self.state.permission_hub.resolve(&pending.id, allow).await;
        let text = match result {
            Ok(()) if allow => format!("✔ Approved: `{}`", pending.command),
            Ok(()) => format!("✘ Denied: `{}`", pending.command),
            Err(e) => format!("⚠️ Could not answer permission request: {e}"),
        };
        self.state.messages.push(UiMessage::Assistant {
            content: text,
            timestamp: Utc::now().timestamp(),
        });
    }
}

/// Run the TUI event loop.
///
/// # Flow
///
/// 1. Enter crossterm raw mode and set up the alternate screen.
/// 2. Initialize ratatui terminal.
/// 3. Enter the main loop:
///    a. Poll for crossterm events (keyboard, mouse, resize).
///    b. Check for incoming `StreamEvent` values from the forge channel.
///    c. Dispatch actions to `AppState::handle_action`.
///    d. Render the UI via `ui::render`.
/// 4. On quit, restore the terminal and exit.
pub async fn run(app: &mut App, mut terminal: DefaultTerminal) -> Result<()> {
    loop {
        // ── Render the current frame ──
        let _ = terminal.draw(|frame| {
            ui::render(frame, &app.state);
        });

        // ── Check for forge events if one is running ──
        if let Some(ref mut rx) = app.event_rx {
            loop {
                match rx.try_recv() {
                    Ok(event) => {
                        app.state.handle_stream_event(event);
                    }
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        // Forge finished (normally or with an error) — finalize.
                        // `is_streaming` must be cleared here: the forge returns
                        // without emitting `Complete` on every fatal path, and a
                        // stale `true` would re-issue the same request forever.
                        app.state.is_streaming = false;
                        app.event_rx = None;
                        app.forge_handle = None;
                        break;
                    }
                }
            }
        }

        // ── Check if forge handle completed ──
        if let Some((session_id, handle)) = app.forge_handle.take() {
            if handle.is_finished() {
                // Already handled via channel disconnect above.
                app.state.is_streaming = false;
            } else {
                // Put it back.
                app.forge_handle = Some((session_id, handle));
            }
        }

        // ── Poll for user input ──
        if event::poll(std::time::Duration::from_millis(16))? {
            let ev = event::read()?;

            // A pending permission prompt owns y/n (and Esc = deny) so the
            // answer never lands in the input buffer.
            let mut consumed = false;
            if !app.state.pending_permissions.is_empty() {
                if let CrosstermEvent::Key(key) = &ev {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            app.resolve_pending_permission(true).await;
                            consumed = true;
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                            app.resolve_pending_permission(false).await;
                            consumed = true;
                        }
                        _ => {}
                    }
                }
            }

            if !consumed {
                match ev {
                    CrosstermEvent::Key(key) => {
                        let ctx = KeyContext {
                            sidebar_selected: app.state.session_select_index.is_some(),
                            input_empty: app.state.input_buffer.is_empty(),
                        };
                        let action = key_event_to_action(key, ctx);
                        app.state.handle_action(action);
                    }
                    CrosstermEvent::Mouse(mouse) => {
                        let action = mouse_event_to_action(mouse);
                        app.state.handle_action(action);
                    }
                    CrosstermEvent::Resize(_, _) => {
                        // Terminal resize — the next render pass will adapt.
                    }
                    _ => {}
                }
            }
        }

        // ── Check exit condition before the launch block ──
        // Must come first: a failing launch must never trap the user in a loop
        // where Escape/Ctrl+Q is not consulted.
        if app.state.should_quit {
            break;
        }

        // ── If Submit action triggered, launch forge ──
        if app.state.is_streaming && app.forge_handle.is_none() {
            let (session_id, message, history) = {
                // submit_input guarantees a session, but never panic the whole
                // UI if that invariant is ever broken.
                let Some(session) = app.state.active_session.as_ref() else {
                    app.state.is_streaming = false;
                    continue;
                };

                let message = app
                    .state
                    .messages
                    .iter()
                    .rev()
                    .find_map(|m| match m {
                        UiMessage::User { content, .. } => Some(content.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();

                let history = session
                    .messages
                    .iter()
                    .filter(|m| matches!(m.role, Role::User | Role::Assistant))
                    .cloned()
                    .collect::<Vec<_>>();

                (session.id.clone(), message, history)
            };
            let session_id_for_handle = session_id.clone();

            let provider = match create_provider(&app.state.model_name, &app.state.config) {
                Ok(p) => p,
                Err(e) => {
                    app.state.messages.push(UiMessage::Error {
                        content: format!("Provider error: {e}"),
                    });
                    // Stop streaming, otherwise every 16 ms tick appends another
                    // copy of this error and relaunches nothing.
                    app.state.is_streaming = false;
                    app.event_rx = None;
                    continue;
                }
            };

            // Build the agent the way the desktop/web front-end does
            // (`dscode-server/src/commands/chat.rs`), so the user's safety
            // config, global prompt, context config and memory recall all apply.
            let (system_prompt, context_cfg, safety_guard, perm_timeout, teams_cfg) = {
                let config = &app.state.config;
                let mut system_prompt = config
                    .agent
                    .resolve_system_prompt(dscode_core::agent::forge::DEFAULT_SYSTEM_PROMPT);
                if config.agent.memory_enabled {
                    // Scoped to this conversation — the FTS index is global, so
                    // an unscoped scribe recalls nothing (see `Scribe::recall`).
                    if let Ok(scribe) =
                        dscode_core::memory::scribe::Scribe::for_session(session_id.clone())
                    {
                        let hits = scribe.recall(&message, 6);
                        if !hits.is_empty() {
                            system_prompt.push_str("\n\n## Memory recall (optional context)\n");
                            for h in hits {
                                system_prompt.push_str("- ");
                                system_prompt.push_str(&h);
                                system_prompt.push('\n');
                            }
                        }
                    }
                }
                (
                    system_prompt,
                    config.context.clone(),
                    Arc::new(SafetyGuard::from_config(config)),
                    config.safety.permission_timeout_secs.max(10),
                    config.teams.clone(),
                )
            };

            // Set up the forge channel.
            let (tx, rx) = mpsc::unbounded_channel::<StreamEvent>();
            app.event_rx = Some(rx);

            let forge = Arc::new(
                Forge::new(
                    provider,
                    app.state.tool_registry.clone(),
                    app.state.working_dir.clone(),
                )
                .with_system_prompt(system_prompt)
                .with_context_config(context_cfg)
                .with_safety_guard(safety_guard)
                .with_permission_hub(app.state.permission_hub.clone())
                .with_permission_timeout(perm_timeout)
                .with_teams_config(teams_cfg),
            );

            let handle = tokio::spawn(async move {
                let _ = forge
                    .execute(&message, &session_id, history, tx)
                    .await;
            });

            app.forge_handle = Some((session_id_for_handle, handle));
        }
    }

    Ok(())
}
