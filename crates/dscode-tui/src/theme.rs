//! Dark color theme for the DS Code TUI.
//!
//! Defines a cohesive color palette inspired by modern terminal IDEs
//! (VS Code Dark+, JetBrains Dark, etc.) using ratatui's `Color` type.

use ratatui::style::Color;

/// Primary theme structure holding all semantic color roles.
pub struct Theme;

impl Theme {
    // ── Background palette ──
    pub const BG: Color = Color::Rgb(0x1E, 0x1E, 0x2E);
    pub const BG_DARK: Color = Color::Rgb(0x18, 0x18, 0x25);
    pub const BG_LIGHT: Color = Color::Rgb(0x28, 0x28, 0x3E);
    pub const BG_HIGHLIGHT: Color = Color::Rgb(0x33, 0x33, 0x4A);

    // ── Foreground / text ──
    pub const TEXT: Color = Color::Rgb(0xCD, 0xD6, 0xF4);
    pub const TEXT_DIM: Color = Color::Rgb(0x6C, 0x70, 0x86);
    pub const TEXT_MUTED: Color = Color::Rgb(0x45, 0x48, 0x5A);

    // ── Accent ──
    pub const ACCENT: Color = Color::Rgb(0x89, 0xB4, 0xFA);
    pub const ACCENT_DIM: Color = Color::Rgb(0x46, 0x69, 0xBF);

    // ── Status colors ──
    pub const SUCCESS: Color = Color::Rgb(0xA6, 0xE3, 0xA1);
    pub const ERROR: Color = Color::Rgb(0xF3, 0x8B, 0xA8);
    pub const WARNING: Color = Color::Rgb(0xF9, 0xE2, 0xAF);
    pub const INFO: Color = Color::Rgb(0x89, 0xDC, 0xEB);

    // ── Tool call card colors ──
    pub const TOOL_BG: Color = Color::Rgb(0x25, 0x2E, 0x3E);
    pub const TOOL_BORDER: Color = Color::Rgb(0x45, 0x52, 0x6E);

    // ── Thinking block colors ──
    pub const THINK_BG: Color = Color::Rgb(0x1E, 0x22, 0x2A);
    pub const THINK_FG: Color = Color::Rgb(0x58, 0x5B, 0x70);

    // ── Sidebar colors ──
    pub const SIDEBAR_BG: Color = Color::Rgb(0x16, 0x18, 0x22);

    // ── Input bar ──
    pub const INPUT_BG: Color = Color::Rgb(0x14, 0x16, 0x1E);
    pub const INPUT_BORDER: Color = Color::Rgb(0x3B, 0x3D, 0x52);

    // ── Status bar ──
    pub const STATUS_BG: Color = Color::Rgb(0x14, 0x16, 0x1E);

    // ── Borders ──
    pub const BORDER: Color = Color::Rgb(0x3B, 0x3D, 0x52);
    pub const BORDER_ACTIVE: Color = Color::Rgb(0x89, 0xB4, 0xFA);
}

// ── ANSI constants for terminal setup ──
impl Theme {
    /// ANSI escape sequence to reset styling.
    pub const RESET_ANSI: &str = "\x1b[0m";
}
