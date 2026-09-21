//! DS Code TUI — Claude Code-style terminal agent interface.
//!
//! Usage:
//!   dscode-tui                     # launch interactive TUI
//!   dscode-tui --model deepseek-v4-flash   # select model
//!   dscode-tui --dir /path/to/project      # set working directory
//!   dscode-tui --help                       # show help

mod app;
mod events;
mod theme;
mod ui;

use anyhow::Result;
use argh::FromArgs;
use crossterm::{
    cursor,
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::app::App;

/// True while the terminal is in raw mode on the alternate screen, so the
/// panic hook knows it has something to undo.
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Undo raw mode / alternate screen / hidden cursor. Safe to call twice and
/// safe to call from a panic hook (never panics itself).
fn restore_terminal() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
}

/// DS Code — Claude Code-style terminal agent interface.
#[derive(FromArgs)]
struct Cli {
    /// model name (e.g. deepseek-v4-pro, deepseek-v4-flash, openai/gpt-4o)
    #[argh(option, short = 'm')]
    model: Option<String>,

    /// working directory override (default: current directory)
    #[argh(option, short = 'd')]
    dir: Option<String>,

    /// print version and exit
    #[argh(switch)]
    version: bool,
}

/// Parse arguments and handle the exit-early flags **outside** the Tokio
/// runtime.
///
/// `argh::from_env` prints usage and calls `process::exit(0)` for `--help`.
/// Exiting from inside a running multi-threaded runtime races with its worker
/// threads and aborts the process — `fatal runtime error: current thread handle
/// already set during thread spawn` — so `dscode-tui --help` printed the usage
/// text correctly and then died with 0xC0000409 instead of exiting 0. Parsing
/// (and therefore exiting) before the runtime exists keeps that path clean.
fn main() -> Result<()> {
    let cli: Cli = argh::from_env();

    if cli.version {
        println!("dscode-tui {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    run(cli)
}

#[tokio::main]
async fn run(cli: Cli) -> Result<()> {
    // ── Panic hook ──
    // A panic inside the event loop or a render pass unwinds out of `app::run`,
    // so the cleanup at the bottom of `main` never executes and the user is
    // left with a raw-mode alternate screen (they have to run `reset`). Restore
    // the terminal from the hook before the default hook prints the message.
    // Only for panics on the main thread — a panic in a spawned forge task must
    // not tear down the terminal of a still-running TUI.
    let main_thread = std::thread::current().id();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == main_thread
            && TERMINAL_ACTIVE.swap(false, Ordering::SeqCst)
        {
            restore_terminal();
        }
        default_hook(info);
    }));

    // ── App setup ──
    let app_result = App::new();
    let mut app = match app_result {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Failed to initialize: {}", e);
            eprintln!("Make sure ~/.dscode/config.toml exists (run dscode-cli once).");
            return Err(e);
        }
    };

    // Apply CLI overrides.
    if let Some(ref model) = cli.model {
        app.state.model_name = model.clone();
    }
    if let Some(ref dir) = cli.dir {
        app.state.working_dir = std::path::PathBuf::from(dir);
    }

    // ── Terminal setup ──
    // Before entering raw mode: a zh-CN console defaults to CP936, and crossterm
    // writes UTF-8, so every han character would render as 乱码 no matter how
    // correctly the app produced it.
    dscode_core::platform::enable_utf8_console();
    let stdout = io::stdout();
    let setup = (|| -> io::Result<()> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;
        Ok(())
    })();
    if let Err(e) = setup {
        restore_terminal();
        return Err(e.into());
    }
    TERMINAL_ACTIVE.store(true, Ordering::SeqCst);

    let terminal = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;

    // ── Run the event loop ──
    let run_result = app::run(&mut app, terminal).await;

    // ── Terminal cleanup (always run, even on error) ──
    TERMINAL_ACTIVE.store(false, Ordering::SeqCst);
    restore_terminal();

    run_result
}
