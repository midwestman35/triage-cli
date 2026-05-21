//! TUI module.
//!
//! - `inbox` — five-markdown ticket-folder viewer (spec § 4, § 10 dec. 4)
//!   plus the background Zendesk poll / on-demand triage controls.
//!
//! The legacy `investigate` TUI (three-pane live progress for
//! `investigate --tui` / `triage --tui`) was removed in the v1 reframe:
//! the structured pipeline does not emit a synchronous `TuiEvent::Done`
//! payload anymore, and the surface had no v1 caller. The bail messages
//! in `cli.rs` for `--tui` flags stay as a useful "this was here, now
//! removed" signal.

pub mod chat;
pub mod effects;
pub mod inbox;

use std::io::{self, Stdout};

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub fn enter_terminal() -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

pub fn leave_terminal(mut terminal: Tui) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

// Re-export so callers can use `crate::tui::run_inbox` without learning the
// submodule layout.
pub use inbox::run_inbox;
