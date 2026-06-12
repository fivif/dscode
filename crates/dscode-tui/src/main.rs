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

use crate::app::App;

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

#[tokio::main]
async fn main() -> Result<()> {
    let cli: Cli = argh::from_env();

    if cli.version {
        println!("dscode-tui {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

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
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
    terminal::enable_raw_mode()?;

    let terminal = ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(stdout))?;

    // ── Run the event loop ──
    let run_result = app::run(&mut app, terminal).await;

    // ── Terminal cleanup (always run, even on error) ──
    let mut cleanup_stdout = io::stdout();
    let _ = execute!(cleanup_stdout, LeaveAlternateScreen, cursor::Show);
    let _ = terminal::disable_raw_mode();

    run_result
}
