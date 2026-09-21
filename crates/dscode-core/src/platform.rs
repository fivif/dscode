//! Windows console setup shared by the terminal front-ends.
//!
//! On a Chinese-locale Windows the console defaults to **CP936**, but the CLI
//! and the TUI (ratatui/crossterm) write **UTF-8**. The console then decodes
//! those bytes as GBK, and every han character renders as 乱码 — regardless of
//! how cleanly the application produced it. Raising both console code pages to
//! UTF-8 is what the symptom actually calls for; it was missing entirely, and
//! nothing in the child-process plumbing can compensate for it.
//!
//! This is a no-op on non-Windows platforms.

/// Switch the attached console to UTF-8 (code page 65001) for both input and
/// output.
///
/// Best-effort by design: a process with no console attached (the Tauri desktop
/// app, a redirected stdout) has nothing to configure, and the call simply
/// fails — which is not an error worth surfacing.
pub fn enable_utf8_console() {
    #[cfg(windows)]
    {
        // Declared via FFI rather than a `windows-sys` dependency: these are two
        // flat kernel32 exports with no types to keep in sync, and the crate
        // already links against kernel32 on this target.
        #[link(name = "kernel32")]
        extern "system" {
            fn SetConsoleOutputCP(code_page: u32) -> i32;
            fn SetConsoleCP(code_page: u32) -> i32;
        }

        const CP_UTF8: u32 = 65001;
        // SAFETY: both are plain `u32 -> i32` kernel32 exports with no pointer
        // arguments; the return value is only a success flag, checked by nothing
        // because a console-less process legitimately fails here.
        unsafe {
            SetConsoleOutputCP(CP_UTF8);
            SetConsoleCP(CP_UTF8);
        }
    }
}
