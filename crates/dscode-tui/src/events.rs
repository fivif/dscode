//! Key and mouse event → action mapping.
//!
//! Translates raw crossterm events into semantic `Action` values that
//! the app loop can handle declaratively.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

/// State that changes how a key is interpreted.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyContext {
    /// A session is highlighted in the sidebar.
    pub sidebar_selected: bool,
    /// The input buffer is empty, so Enter can act on the sidebar selection
    /// instead of submitting a message.
    pub input_empty: bool,
}

/// Semantic actions produced by user input.
#[derive(Debug, Clone)]
pub enum Action {
    /// Quit the application.
    Quit,

    /// Create a new chat session.
    NewChat,

    /// Open the settings popup (or toggle if already open).
    Settings,

    /// Toggle the sidebar visibility.
    ToggleSidebar,

    /// Submit the current input buffer for processing.
    Submit,

    /// Delete one character before the cursor.
    Backspace,

    /// Delete one character after the cursor.
    Delete,

    /// Move the cursor left in the input buffer.
    CursorLeft,

    /// Move the cursor right in the input buffer.
    CursorRight,

    /// Move the cursor to the beginning of the input line.
    CursorHome,

    /// Move the cursor to the end of the input line.
    CursorEnd,

    /// Insert a character into the input buffer at the cursor.
    InsertChar(char),

    /// Insert a newline into the input buffer.
    InsertNewline,

    /// Navigate session list up.
    SessionUp,

    /// Navigate session list down.
    SessionDown,

    /// Select the highlighted session.
    SessionSelect,

    /// Delete the highlighted session.
    SessionDelete,

    /// Scroll chat view up one line.
    ScrollUp,

    /// Scroll chat view down one line.
    ScrollDown,

    /// Page up in the chat view.
    ScrollPageUp,

    /// Page down in the chat view.
    ScrollPageDown,

    /// Scroll to the bottom of the chat view.
    ScrollBottom,

    /// Toggle the collapse state of a thinking block.
    ToggleThinking(usize),

    /// Toggle the collapse state of the most recent thinking block.
    ToggleLatestThinking,

    /// Toggle the collapse state of a tool card.
    ToggleToolCard(usize),

    /// Toggle the collapse state of the most recent tool card.
    ToggleLatestToolCard,

    /// Go to the previous message in input history.
    HistoryPrevious,

    /// Go to the next message in input history.
    HistoryNext,

    /// No-op / unrecognized input.
    Noop,
}

/// Convert a crossterm `KeyEvent` into an `Action`.
pub fn key_event_to_action(key: KeyEvent, ctx: KeyContext) -> Action {
    use KeyCode::*;

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // Sidebar management (only meaningful when something is selected).
    if ctrl {
        match key.code {
            Char('d') | Char('D') => return Action::SessionDelete,
            Char('t') | Char('T') => return Action::ToggleLatestThinking,
            Char('o') | Char('O') => return Action::ToggleLatestToolCard,
            End => return Action::ScrollBottom,
            _ => {}
        }
    }

    // Global shortcuts take priority.
    match key {
        KeyEvent {
            code: Char('c'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } if cfg!(not(debug_assertions)) => Action::Quit,
        KeyEvent {
            code: Char('q'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Action::Quit,
        KeyEvent {
            code: Esc, ..
        } => Action::Quit,

        KeyEvent {
            code: Char('n'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Action::NewChat,
        KeyEvent {
            code: Char('s'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Action::Settings,
        KeyEvent {
            code: Char('l'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Action::ToggleSidebar,

        // Input area keys.
        KeyEvent {
            code: Enter, ..
        } => {
            // Shift+Enter inserts a newline. With an empty input and a session
            // highlighted in the sidebar, Enter opens that session — otherwise
            // the session list would be unreachable.
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                Action::InsertNewline
            } else if ctx.sidebar_selected && ctx.input_empty {
                Action::SessionSelect
            } else {
                Action::Submit
            }
        }
        KeyEvent {
            code: Backspace, ..
        } => Action::Backspace,
        KeyEvent {
            code: Delete, ..
        } => Action::Delete,
        KeyEvent {
            code: Left, ..
        } => Action::CursorLeft,
        KeyEvent {
            code: Right, ..
        } => Action::CursorRight,
        KeyEvent {
            code: Home, ..
        } => Action::CursorHome,
        KeyEvent {
            code: End, ..
        } => Action::CursorEnd,

        // Navigation keys.
        KeyEvent {
            code: Up, ..
        } => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                Action::SessionUp
            } else if key.modifiers.contains(KeyModifiers::ALT) {
                Action::ScrollUp
            } else {
                Action::HistoryPrevious
            }
        }
        KeyEvent {
            code: Down, ..
        } => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                Action::SessionDown
            } else if key.modifiers.contains(KeyModifiers::ALT) {
                Action::ScrollDown
            } else {
                Action::HistoryNext
            }
        }

        KeyEvent {
            code: PageUp, ..
        } => Action::ScrollPageUp,
        KeyEvent {
            code: PageDown, ..
        } => Action::ScrollPageDown,

        KeyEvent {
            code: Tab, ..
        } => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                Action::CursorLeft
            } else {
                Action::ToggleSidebar
            }
        }

        // Character input.
        KeyEvent {
            code: Char(c), ..
        } => Action::InsertChar(c),

        _ => Action::Noop,
    }
}

/// Convert a crossterm `MouseEvent` into an `Action`.
pub fn mouse_event_to_action(event: MouseEvent) -> Action {
    match event.kind {
        MouseEventKind::ScrollUp => Action::ScrollUp,
        MouseEventKind::ScrollDown => Action::ScrollDown,
        _ => Action::Noop,
    }
}
