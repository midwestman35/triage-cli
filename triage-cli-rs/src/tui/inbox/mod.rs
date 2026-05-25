//! Inbox TUI over the five-markdown ticket-folder corpus (spec § 4 + § 10
//! decision 4).
//!
//! The module is split by responsibility: `mod.rs` owns the event loop,
//! `app.rs` owns state/actions, `poll.rs` owns Zendesk + pipeline orchestration,
//! `state.rs` owns STATE.md parsing, `render.rs` owns rendering helpers, and
//! `chat.rs` owns the chat session loop.

mod app;
mod chat;
mod poll;
mod render;
mod state;

use std::io;
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use crossterm::event::{self, Event, KeyEvent};
use tokio::sync::mpsc;

use self::app::{InboxApp, NotifyKind};
use self::chat::run_chat_session;
use crate::playbook::Rubric;
use crate::ticket_folder::tickets_root;
use crate::watcher::{self, WatcherOptions};

pub use self::app::{RowEntry, Status};
pub use self::render::{render_synth_summary, rubric_mismatch, rubric_mismatch_banner};
pub use self::state::{parse_state_md, parse_state_md_str, scan_tickets_root, InboxStateSummary};

const TABS: &[&str] = &[
    "INTAKE.md",
    "EVIDENCE_PREFLIGHT.md",
    "FORK_PACKET.md",
    "DRAFTS.md",
    "STATE.md",
];

/// Public entry point.
pub async fn run_inbox(opts: WatcherOptions) -> io::Result<()> {
    let rubric =
        Rubric::load().map_err(|e| io::Error::other(format!("could not load fork rubric: {e}")))?;
    let tickets_root = tickets_root();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();

    let initial_state = watcher::load_state(&opts.state_file).unwrap_or_default();
    let backfill_cutoff = if opts.backfill_hours.is_finite() {
        Utc::now() - ChronoDuration::hours(opts.backfill_hours as i64)
    } else {
        DateTime::<Utc>::MIN_UTC
    };

    let mut app = InboxApp::new(
        opts.clone(),
        tickets_root,
        rubric,
        initial_state,
        backfill_cutoff,
        event_tx.clone(),
    );
    app.hydrate_from_disk();

    let mut terminal = super::enter_terminal()?;
    app.spawn_poll();

    let interval = Duration::from_secs(opts.interval.max(10));
    let mut poll_ticker = tokio::time::interval(interval);
    poll_ticker.tick().await;

    loop {
        while let Ok(ev) = event_rx.try_recv() {
            app.handle_event(ev);
        }

        let now = Instant::now();
        app.tick_effects(now.duration_since(app.last_frame()));
        app.set_last_frame(now);

        terminal.draw(|f| app.draw(f))?;

        let key_timeout = if app.wants_animation_frame() {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(120)
        };
        let key_event = poll_key_event(key_timeout);
        tokio::select! {
            biased;
            ev = event_rx.recv() => {
                if let Some(ev) = ev {
                    app.handle_event(ev);
                }
            }
            _ = poll_ticker.tick() => {
                app.spawn_poll();
            }
            ke = key_event => {
                if let Ok(Some(key)) = ke {
                    app.handle_key(key);
                    app.maybe_clear_modal();
                }
            }
        }

        app.expire_notifications(Instant::now());

        if app.should_exit() {
            break;
        }

        if let Some(ticket_id) = app.take_pending_chat_ticket_id() {
            suspend_for_chat(&mut terminal)?;
            let chat_result = run_chat_session(&ticket_id).await;
            resume_after_chat(&mut terminal)?;

            match chat_result {
                Ok(()) => app.notify(
                    format!("Chat session for #{ticket_id} closed"),
                    NotifyKind::Info,
                ),
                Err(e) => app.notify(format!("Chat error: {e}"), NotifyKind::Error),
            }
        }
    }

    let _ = watcher::save_state(&opts.state_file, app.state());
    app.abort_pending_triages();
    super::leave_terminal(terminal)?;
    Ok(())
}

fn suspend_for_chat(terminal: &mut super::Tui) -> io::Result<()> {
    use crossterm::{
        event::DisableMouseCapture,
        execute,
        terminal::{disable_raw_mode, LeaveAlternateScreen},
    };

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn resume_after_chat(terminal: &mut super::Tui) -> io::Result<()> {
    use crossterm::{
        event::EnableMouseCapture,
        execute,
        terminal::{enable_raw_mode, EnterAlternateScreen},
    };

    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;
    terminal.clear()
}

async fn poll_key_event(timeout: Duration) -> io::Result<Option<KeyEvent>> {
    tokio::task::spawn_blocking(move || -> io::Result<Option<KeyEvent>> {
        if event::poll(timeout)? {
            if let Event::Key(k) = event::read()? {
                return Ok(Some(k));
            }
        }
        Ok(None)
    })
    .await
    .map_err(|e| io::Error::other(e.to_string()))?
}
