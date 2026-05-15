//! Inbox TUI over the five-markdown ticket-folder corpus (spec § 4 + § 10
//! decision 4).
//!
//! Behavior:
//!   - Scans `tickets_root()` for subdirectories that contain a `STATE.md`.
//!     Each such directory is one inbox row.
//!   - Default view (right pane) is a synthetic single-pane summary —
//!     fork letter, confidence, quoted rubric row, status, owner, related
//!     tickets — parsed out of `STATE.md`.
//!   - Pressing `Tab` switches to the per-file tabbed view across
//!     INTAKE / EVIDENCE_PREFLIGHT / FORK_PACKET / DRAFTS / STATE; `Tab`
//!     cycles tabs. `Esc` returns to the synth view.
//!   - When the selected ticket's `STATE.md` carries a `rubric_version`
//!     that does not match the shipped rubric, a non-blocking yellow
//!     banner is shown above the content.
//!   - Background polling of the configured Zendesk view continues to run;
//!     `Enter` on a queued ticket triggers `investigate_one_structured`.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Gauge, Paragraph, Row, Table, Wrap},
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::{enter_terminal, leave_terminal};
use crate::datadog::DatadogClient;
use crate::extract;
use crate::investigation;
use crate::models::Ticket;
use crate::pipeline::{self, InvestigateOptions, Reporter, StructuredInvestigation};
use crate::playbook::Rubric;
use crate::ticket_folder::{self, tickets_root};
use crate::watcher::{self, State, WatcherOptions};
use crate::zendesk::ZendeskClient;

const SELECTED_ICON: &str = "◉";
const NOTIFY_TTL: Duration = Duration::from_secs(4);
const PHASES_TOTAL: u8 = 4;

/// The five files in a ticket folder, in display order.
const TABS: &[&str] = &[
    "INTAKE.md",
    "EVIDENCE_PREFLIGHT.md",
    "FORK_PACKET.md",
    "DRAFTS.md",
    "STATE.md",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Triaged,
    Triaging,
    Queued,
    Failed,
}

impl Status {
    fn priority(&self) -> u8 {
        match self {
            Status::Triaging => 0,
            Status::Triaged => 1,
            Status::Queued => 2,
            Status::Failed => 3,
        }
    }
    fn icon(&self) -> char {
        match self {
            Status::Triaged => '✓',
            Status::Triaging => '→',
            Status::Queued => '○',
            Status::Failed => '✗',
        }
    }
    fn label(&self) -> &'static str {
        match self {
            Status::Triaged => "triaged",
            Status::Triaging => "triaging…",
            Status::Queued => "in queue",
            Status::Failed => "failed",
        }
    }
    /// Background tint that paints the row when the status is loud.
    fn row_bg(&self) -> Option<Color> {
        match self {
            Status::Failed => Some(Color::Rgb(60, 0, 0)),
            Status::Triaging => Some(Color::Rgb(60, 40, 0)),
            _ => None,
        }
    }
}

/// Parsed `STATE.md` frontmatter — superset of `ticket_folder::ExistingState`,
/// with the `related` block also parsed so we can render related Zendesk/Jira
/// IDs in the synth summary.
#[derive(Debug, Clone, Default)]
pub struct InboxStateSummary {
    pub ticket_id: Option<u64>,
    pub fork: Option<String>,
    pub confidence: Option<String>,
    pub status: Option<String>,
    pub owner: Option<String>,
    pub quoted_rubric_row: Option<String>,
    pub rubric_version: Option<String>,
    pub related_zendesk: Vec<u64>,
    pub related_jira: Vec<String>,
    pub master: Option<u64>,
    pub cluster: Option<String>,
    pub validator_warnings: Vec<String>,
    pub updated_at: Option<DateTime<Utc>>,
}

/// One on-disk ticket folder. Read lazily; the only field eagerly populated
/// is `state` (so the row list can render summary columns).
#[derive(Debug, Clone)]
pub struct RowEntry {
    pub ticket_id: u64,
    pub status: Status,
    pub folder: Option<PathBuf>,
    pub state: Option<InboxStateSummary>,
    pub site_hint: Option<String>,
    pub failure_reason: Option<String>,
    pub phase_label: Option<String>,
    pub phase_step: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Detail,
}

/// What the right pane shows for a triaged ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailMode {
    /// Synth single-pane summary (default).
    Summary,
    /// Tabbed per-file viewer; the `usize` is an index into `TABS`.
    File(usize),
}

#[derive(Debug, Clone, Copy)]
enum NotifyKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
struct Notification {
    kind: NotifyKind,
    text: String,
    expires_at: Instant,
}

#[derive(Debug)]
enum InboxEvent {
    PollStarted,
    PollFinished {
        new_state: State,
        view_ids: HashSet<u64>,
    },
    PollFailed {
        msg: String,
    },
    TriagePhase {
        ticket_id: u64,
        label: String,
        step: u8,
    },
    TriageComplete {
        ticket_id: u64,
        folder: PathBuf,
    },
    TriageFailed {
        ticket_id: u64,
        error: String,
    },
    SiteInputNeeded {
        ticket_id: u64,
        subject: String,
        org: Option<String>,
        responder: oneshot::Sender<Option<String>>,
    },
}

struct SiteInputModal {
    ticket_id: u64,
    subject: String,
    org: Option<String>,
    input: String,
    responder: Option<oneshot::Sender<Option<String>>>,
}

struct InboxApp {
    opts: WatcherOptions,
    tickets_root: PathBuf,
    rubric: Rubric,
    rows: BTreeMap<u64, RowEntry>,
    state: State,
    backfill_cutoff: DateTime<Utc>,
    view_ids: HashSet<u64>,
    cursor: usize,
    focus: Focus,
    detail_mode: DetailMode,
    polling: bool,
    last_poll: Option<DateTime<Utc>>,
    notification: Option<Notification>,
    report_scroll: u16,
    modal: Option<SiteInputModal>,
    pending_triages: Vec<JoinHandle<()>>,
    event_tx: mpsc::UnboundedSender<InboxEvent>,
    should_exit: bool,
}

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

    let mut app = InboxApp {
        opts: opts.clone(),
        tickets_root: tickets_root.clone(),
        rubric,
        rows: BTreeMap::new(),
        state: initial_state,
        backfill_cutoff,
        view_ids: HashSet::new(),
        cursor: 0,
        focus: Focus::List,
        detail_mode: DetailMode::Summary,
        polling: false,
        last_poll: None,
        notification: None,
        report_scroll: 0,
        modal: None,
        pending_triages: Vec::new(),
        event_tx: event_tx.clone(),
        should_exit: false,
    };
    app.hydrate_from_disk();

    let mut terminal = enter_terminal()?;

    // Kick off the first poll immediately.
    app.spawn_poll();

    // Periodic poll ticker.
    let interval = Duration::from_secs(opts.interval.max(10));
    let mut poll_ticker = tokio::time::interval(interval);
    poll_ticker.tick().await; // discard the immediate first tick

    loop {
        // Drain any pending pipeline events without blocking.
        while let Ok(ev) = event_rx.try_recv() {
            app.handle_event(ev);
        }

        terminal.draw(|f| app.draw(f))?;

        // Wait for: keypress, ticker, or pipeline event — whichever fires first.
        let key_event = poll_key_event(Duration::from_millis(120));
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

        // Expire stale notifications.
        if let Some(n) = &app.notification {
            if n.expires_at <= Instant::now() {
                app.notification = None;
            }
        }

        if app.should_exit {
            break;
        }
    }

    // Persist state before tearing down. `poll_iteration` already pruned
    // the state with the live_set in scope, so no re-prune is needed here;
    // re-pruning without a live_set would risk evicting in-view tickets
    // dormant past the TTL and re-triage them on next poll.
    let _ = watcher::save_state(&opts.state_file, &app.state);
    for handle in app.pending_triages.drain(..) {
        handle.abort();
    }
    leave_terminal(terminal)?;
    Ok(())
}

/// Poll keys with a timeout so the outer `select!` always has a pending future.
async fn poll_key_event(timeout: Duration) -> io::Result<Option<KeyEvent>> {
    // crossterm's `poll`/`read` are synchronous; defer them onto a blocking
    // thread so they don't stall the async runtime.
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

impl InboxApp {
    /// Scan `tickets_root` for subdirectories with a `STATE.md` and seed
    /// the inbox rows from them.
    fn hydrate_from_disk(&mut self) {
        for (id, folder, summary) in scan_tickets_root(&self.tickets_root) {
            self.rows.insert(
                id,
                RowEntry {
                    ticket_id: id,
                    status: Status::Triaged,
                    folder: Some(folder),
                    state: Some(summary),
                    site_hint: None,
                    failure_reason: None,
                    phase_label: None,
                    phase_step: 0,
                },
            );
        }
    }

    fn sorted_rows(&self) -> Vec<RowEntry> {
        let mut rows: Vec<RowEntry> = self.rows.values().cloned().collect();
        rows.sort_by(|a, b| {
            let pa = a.status.priority();
            let pb = b.status.priority();
            pa.cmp(&pb).then_with(|| {
                let ta = a
                    .state
                    .as_ref()
                    .and_then(|s| s.updated_at)
                    .unwrap_or(DateTime::<Utc>::MIN_UTC);
                let tb = b
                    .state
                    .as_ref()
                    .and_then(|s| s.updated_at)
                    .unwrap_or(DateTime::<Utc>::MIN_UTC);
                tb.cmp(&ta)
            })
        });
        rows
    }

    fn notify(&mut self, text: impl Into<String>, kind: NotifyKind) {
        self.notification = Some(Notification {
            kind,
            text: text.into(),
            expires_at: Instant::now() + NOTIFY_TTL,
        });
    }

    fn spawn_poll(&mut self) {
        if self.polling {
            return;
        }
        if self.modal.is_some() {
            return;
        }
        self.polling = true;
        let tx = self.event_tx.clone();
        let opts = self.opts.clone();
        let backfill_cutoff = self.backfill_cutoff;
        let state_snapshot = State {
            version: self.state.version,
            triaged: self.state.triaged.clone(),
        };
        let _ = tx.send(InboxEvent::PollStarted);
        let handle = tokio::spawn(async move {
            match poll_iteration(state_snapshot, opts, backfill_cutoff, tx.clone()).await {
                Ok((new_state, view_ids)) => {
                    let _ = tx.send(InboxEvent::PollFinished {
                        new_state,
                        view_ids,
                    });
                }
                Err(msg) => {
                    let _ = tx.send(InboxEvent::PollFailed { msg });
                }
            }
        });
        self.pending_triages.push(handle);
    }

    fn reload_row_from_disk(&mut self, ticket_id: u64, folder: PathBuf) {
        let state_path = folder.join("STATE.md");
        let summary = parse_state_md(&state_path).unwrap_or_default();
        self.rows.insert(
            ticket_id,
            RowEntry {
                ticket_id,
                status: Status::Triaged,
                folder: Some(folder),
                state: Some(summary),
                site_hint: None,
                failure_reason: None,
                phase_label: None,
                phase_step: 0,
            },
        );
    }

    fn handle_event(&mut self, ev: InboxEvent) {
        match ev {
            InboxEvent::PollStarted => {
                self.polling = true;
            }
            InboxEvent::PollFinished {
                new_state,
                view_ids,
            } => {
                self.polling = false;
                self.state = new_state;
                // `poll_iteration` already pruned with the live_set; just save.
                let _ = watcher::save_state(&self.opts.state_file, &self.state);
                self.view_ids = view_ids;
                // Insert queued placeholders for any view tickets we haven't
                // already seen as a triaged ticket folder.
                for id in &self.view_ids {
                    self.rows.entry(*id).or_insert(RowEntry {
                        ticket_id: *id,
                        status: Status::Queued,
                        folder: None,
                        state: None,
                        site_hint: None,
                        failure_reason: None,
                        phase_label: None,
                        phase_step: 0,
                    });
                }
                self.last_poll = Some(Utc::now());
            }
            InboxEvent::PollFailed { msg } => {
                self.polling = false;
                self.notify(format!("Poll error: {msg}"), NotifyKind::Error);
                if msg.to_lowercase().contains("view") && msg.to_lowercase().contains("not found") {
                    self.should_exit = true;
                }
            }
            InboxEvent::TriagePhase {
                ticket_id,
                label,
                step,
            } => {
                if let Some(entry) = self.rows.get_mut(&ticket_id) {
                    entry.status = Status::Triaging;
                    entry.phase_label = Some(label);
                    entry.phase_step = step;
                }
            }
            InboxEvent::TriageComplete { ticket_id, folder } => {
                self.reload_row_from_disk(ticket_id, folder);
            }
            InboxEvent::TriageFailed { ticket_id, error } => {
                let entry = self.rows.entry(ticket_id).or_insert(RowEntry {
                    ticket_id,
                    status: Status::Failed,
                    folder: None,
                    state: None,
                    site_hint: None,
                    failure_reason: None,
                    phase_label: None,
                    phase_step: 0,
                });
                entry.status = Status::Failed;
                entry.failure_reason = Some(error);
                entry.phase_label = None;
                entry.phase_step = 0;
            }
            InboxEvent::SiteInputNeeded {
                ticket_id,
                subject,
                org,
                responder,
            } => {
                self.modal = Some(SiteInputModal {
                    ticket_id,
                    subject,
                    org,
                    input: String::new(),
                    responder: Some(responder),
                });
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if self.modal.is_some() {
            handle_modal_key(&mut self.modal, key);
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.should_exit = true;
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => self.cursor_up(),
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => self.cursor_down(),
            (KeyCode::Enter, _) => self.action_enter(),
            (KeyCode::Tab, _) => self.action_cycle_tab(),
            (KeyCode::BackTab, _) => self.action_cycle_tab_back(),
            (KeyCode::Esc, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                if self.detail_mode != DetailMode::Summary {
                    self.detail_mode = DetailMode::Summary;
                    self.report_scroll = 0;
                } else {
                    self.focus = Focus::List;
                    self.report_scroll = 0;
                }
            }
            (KeyCode::Char('r'), _) => self.action_refresh(),
            (KeyCode::Char('y'), _) => self.action_copy_summary(),
            (KeyCode::Char('o'), _) => self.action_open_zendesk(),
            (KeyCode::PageDown, _) => self.report_scroll = self.report_scroll.saturating_add(8),
            (KeyCode::PageUp, _) => self.report_scroll = self.report_scroll.saturating_sub(8),
            _ => {}
        }
    }

    fn cursor_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
        self.report_scroll = 0;
    }

    fn cursor_down(&mut self) {
        let len = self.rows.len();
        if self.cursor + 1 < len {
            self.cursor += 1;
        }
        self.report_scroll = 0;
    }

    fn selected_row(&self) -> Option<RowEntry> {
        self.sorted_rows().into_iter().nth(self.cursor)
    }

    fn action_enter(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        match row.status {
            Status::Queued => {
                self.notify(
                    format!("Starting triage for #{}…", row.ticket_id),
                    NotifyKind::Info,
                );
                self.spawn_triage(row.ticket_id, None);
            }
            _ => {
                self.focus = Focus::Detail;
            }
        }
    }

    fn action_cycle_tab(&mut self) {
        // Tab cycles forward through synth → INTAKE → PREFLIGHT → FORK → DRAFTS → STATE → synth.
        self.detail_mode = match self.detail_mode {
            DetailMode::Summary => DetailMode::File(0),
            DetailMode::File(i) if i + 1 < TABS.len() => DetailMode::File(i + 1),
            DetailMode::File(_) => DetailMode::Summary,
        };
        self.report_scroll = 0;
        self.focus = Focus::Detail;
    }

    fn action_cycle_tab_back(&mut self) {
        self.detail_mode = match self.detail_mode {
            DetailMode::Summary => DetailMode::File(TABS.len() - 1),
            DetailMode::File(0) => DetailMode::Summary,
            DetailMode::File(i) => DetailMode::File(i - 1),
        };
        self.report_scroll = 0;
        self.focus = Focus::Detail;
    }

    fn action_refresh(&mut self) {
        if self.polling {
            self.notify("Already polling", NotifyKind::Info);
            return;
        }
        // Re-scan disk too, so newly-investigated ticket folders show up
        // even when no Zendesk poll has run yet.
        self.hydrate_from_disk();
        self.notify("Refreshing…", NotifyKind::Info);
        self.spawn_poll();
    }

    fn action_copy_summary(&mut self) {
        let Some(row) = self.selected_row() else {
            self.notify("No ticket selected", NotifyKind::Warning);
            return;
        };
        let Some(state) = row.state.as_ref() else {
            self.notify(
                "No STATE.md available for selected ticket",
                NotifyKind::Warning,
            );
            return;
        };
        let text = render_synth_summary(row.ticket_id, state, self.rubric.version()).join("\n");
        if copy_to_clipboard(&text) {
            self.notify("Copied synth summary", NotifyKind::Success);
        } else {
            self.notify(
                "No clipboard tool found (install pbcopy/wl-copy/xclip)",
                NotifyKind::Warning,
            );
        }
    }

    fn action_open_zendesk(&mut self) {
        let Some(row) = self.selected_row() else {
            self.notify("No ticket selected", NotifyKind::Warning);
            return;
        };
        let subdomain = std::env::var("ZENDESK_SUBDOMAIN").unwrap_or_default();
        if subdomain.is_empty() {
            self.notify("ZENDESK_SUBDOMAIN not set", NotifyKind::Warning);
            return;
        }
        let url = format!(
            "https://{subdomain}.zendesk.com/agent/tickets/{}",
            row.ticket_id
        );
        if open_url(&url) {
            self.notify(format!("Opened {url}"), NotifyKind::Info);
        } else {
            self.notify(format!("Could not open: {url}"), NotifyKind::Warning);
        }
    }

    fn spawn_triage(&mut self, ticket_id: u64, site_override: Option<String>) {
        if let Some(entry) = self.rows.get_mut(&ticket_id) {
            entry.status = Status::Triaging;
            entry.phase_label = Some("Fetching ticket".into());
            entry.phase_step = 1;
        }
        let tx = self.event_tx.clone();
        let opts = self.opts.clone();
        let handle = tokio::spawn(async move {
            if let Err(e) = triage_one_ticket(ticket_id, opts, tx.clone(), site_override).await {
                let _ = tx.send(InboxEvent::TriageFailed {
                    ticket_id,
                    error: e,
                });
            }
        });
        self.pending_triages.push(handle);
    }

    fn draw(&mut self, frame: &mut ratatui::Frame) {
        let area = frame.area();
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(8),
                Constraint::Length(1),
            ])
            .split(area);
        self.draw_header(frame, outer[0]);
        self.draw_body(frame, outer[1]);
        self.draw_footer(frame, outer[2]);
        if self.notification.is_some() {
            self.draw_notification(frame, area);
        }
        if self.modal.is_some() {
            self.draw_modal(frame, area);
        }
    }

    fn draw_header(&self, frame: &mut ratatui::Frame, area: Rect) {
        let view_label = match self.opts.view_id {
            Some(id) => id.to_string(),
            None => "my tickets".into(),
        };
        let last = self
            .last_poll
            .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_else(|| "-".into());
        let count = self.rows.len();
        let plural = if count == 1 { "ticket" } else { "tickets" };
        let polling_marker = if self.polling { " · polling…" } else { "" };
        let title = Line::from(vec![
            Span::styled(
                "triage-cli inbox",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  ·  "),
            Span::raw(format!(
                "{view_label} · {count} {plural} · last poll: {last}{polling_marker}"
            )),
        ]);
        let para = Paragraph::new(title).block(Block::default().borders(Borders::BOTTOM));
        frame.render_widget(para, area);
    }

    fn draw_body(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);
        self.draw_list(frame, split[0]);
        self.draw_detail(frame, split[1]);
    }

    fn draw_list(&self, frame: &mut ratatui::Frame, area: Rect) {
        let rows_data = self.sorted_rows();
        let now = Utc::now();
        let header = Row::new(vec![
            Cell::from(" "),
            Cell::from("Ticket"),
            Cell::from("Fork"),
            Cell::from("When"),
            Cell::from("Conf"),
            Cell::from("Owner / Status"),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD));

        let rows: Vec<Row> = rows_data
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let is_selected = i == self.cursor;
                let icon_str = if is_selected {
                    format!("{SELECTED_ICON} {}", row.status.icon())
                } else {
                    format!("  {}", row.status.icon())
                };
                let fork = row
                    .state
                    .as_ref()
                    .and_then(|s| s.fork.clone())
                    .unwrap_or_else(|| "—".into());
                let when = match row.state.as_ref().and_then(|s| s.updated_at) {
                    Some(t) => relative_time(t, now),
                    None => "—".into(),
                };
                let conf = match row.state.as_ref().and_then(|s| s.confidence.clone()) {
                    Some(c) => confidence_cell(&c),
                    None => Cell::from("—"),
                };
                let summary = match row.state.as_ref() {
                    Some(s) => {
                        let status = s.status.clone().unwrap_or_else(|| "open".into());
                        let owner = s.owner.clone().unwrap_or_else(|| "(unowned)".into());
                        truncate(&format!("{owner} · {status}"), 60)
                    }
                    None => row
                        .failure_reason
                        .clone()
                        .unwrap_or_else(|| row.status.label().to_string()),
                };
                let mut style = Style::default();
                if let Some(bg) = row.status.row_bg() {
                    style = style.bg(bg);
                }
                if is_selected {
                    style = style.add_modifier(Modifier::BOLD);
                }
                Row::new(vec![
                    Cell::from(icon_str),
                    Cell::from(format!("#{}", row.ticket_id)),
                    Cell::from(fork),
                    Cell::from(when),
                    conf,
                    Cell::from(summary),
                ])
                .style(style)
            })
            .collect();

        let widths = [
            Constraint::Length(4),
            Constraint::Length(8),
            Constraint::Length(5),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Min(20),
        ];

        let title = if self.focus == Focus::List {
            "Tickets ◀"
        } else {
            "Tickets"
        };
        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(table, area);
    }

    fn draw_detail(&self, frame: &mut ratatui::Frame, area: Rect) {
        let title = match self.detail_mode {
            DetailMode::Summary => "Summary".to_string(),
            DetailMode::File(i) => format!("{} ({}/{})", TABS[i], i + 1, TABS.len()),
        };
        let titled = if self.focus == Focus::Detail {
            format!("{title} ◀")
        } else {
            title
        };
        let outer = Block::default().borders(Borders::ALL).title(titled);
        let inner = outer.inner(area);
        frame.render_widget(outer, area);

        let Some(row) = self.selected_row() else {
            let para = Paragraph::new("Select a ticket to view its report.".dim().to_string())
                .wrap(Wrap { trim: false });
            frame.render_widget(para, inner);
            return;
        };

        match row.status {
            Status::Queued => {
                let para =
                    Paragraph::new("○ In queue — press Enter to triage now.".dim().to_string())
                        .wrap(Wrap { trim: false });
                frame.render_widget(para, inner);
            }
            Status::Triaging => {
                let split = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(2),
                        Constraint::Length(2),
                        Constraint::Min(0),
                    ])
                    .split(inner);
                let label = row
                    .phase_label
                    .clone()
                    .unwrap_or_else(|| "Triaging…".into());
                let label_para = Paragraph::new(label);
                frame.render_widget(label_para, split[0]);
                let ratio = (row.phase_step as f64 / PHASES_TOTAL as f64).min(1.0);
                let gauge = Gauge::default()
                    .ratio(ratio)
                    .gauge_style(Style::default().fg(Color::Yellow))
                    .label(format!("{}/{}", row.phase_step, PHASES_TOTAL));
                frame.render_widget(gauge, split[1]);
            }
            Status::Failed => {
                let msg = format!(
                    "✗ Triage failed:\n\n{}",
                    row.failure_reason.unwrap_or_else(|| "Unknown error".into())
                );
                let para = Paragraph::new(msg.red().to_string()).wrap(Wrap { trim: false });
                frame.render_widget(para, inner);
            }
            Status::Triaged => {
                self.draw_triaged_detail(frame, inner, &row);
            }
        }
    }

    fn draw_triaged_detail(&self, frame: &mut ratatui::Frame, inner: Rect, row: &RowEntry) {
        let mismatch = row
            .state
            .as_ref()
            .and_then(|s| s.rubric_version.as_deref())
            .and_then(|v| {
                let shipped = self.rubric.version();
                if v != shipped {
                    Some((v.to_string(), shipped.to_string()))
                } else {
                    None
                }
            });

        let (banner_area, content_area) = if mismatch.is_some() {
            let split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(0)])
                .split(inner);
            (Some(split[0]), split[1])
        } else {
            (None, inner)
        };

        if let (Some(area), Some((state_v, shipped_v))) = (banner_area, mismatch) {
            let line = rubric_mismatch_banner(&state_v, &shipped_v);
            let banner = Paragraph::new(line).style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            );
            frame.render_widget(banner, area);
        }

        let body = match self.detail_mode {
            DetailMode::Summary => row
                .state
                .as_ref()
                .map(|s| render_synth_summary(row.ticket_id, s, self.rubric.version()).join("\n"))
                .unwrap_or_else(|| "(no STATE.md)".into()),
            DetailMode::File(i) => {
                let file = TABS[i];
                match row.folder.as_ref() {
                    Some(folder) => read_file_for_display(&folder.join(file)),
                    None => format!("(no ticket folder on disk for #{})", row.ticket_id),
                }
            }
        };
        let para = Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .scroll((self.report_scroll, 0));
        frame.render_widget(para, content_area);
    }

    fn draw_footer(&self, frame: &mut ratatui::Frame, area: Rect) {
        let hints =
            "↑/k ↓/j move · enter triage/focus · tab cycle files · esc summary · r refresh · y copy · o open · q quit";
        let para = Paragraph::new(hints.dim().to_string());
        frame.render_widget(para, area);
    }

    fn draw_notification(&self, frame: &mut ratatui::Frame, area: Rect) {
        let Some(n) = &self.notification else {
            return;
        };
        let style = match n.kind {
            NotifyKind::Info => Style::default().fg(Color::Cyan),
            NotifyKind::Success => Style::default().fg(Color::Green),
            NotifyKind::Warning => Style::default().fg(Color::Yellow),
            NotifyKind::Error => Style::default().fg(Color::Red),
        };
        let width = (n.text.chars().count() as u16 + 4).min(area.width.saturating_sub(2));
        let rect = Rect {
            x: area.x + (area.width.saturating_sub(width) / 2),
            y: area.y + area.height.saturating_sub(4),
            width,
            height: 3,
        };
        frame.render_widget(Clear, rect);
        let para = Paragraph::new(n.text.clone())
            .style(style)
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(para, rect);
    }

    fn draw_modal(&self, frame: &mut ratatui::Frame, area: Rect) {
        let Some(modal) = &self.modal else {
            return;
        };
        let width = (area.width.saturating_sub(10)).min(70);
        let height = 10u16;
        let rect = Rect {
            x: area.x + (area.width.saturating_sub(width) / 2),
            y: area.y + (area.height.saturating_sub(height) / 2),
            width,
            height,
        };
        frame.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Site lookup failed");
        frame.render_widget(block, rect);
        let inner = rect.inner(Margin {
            horizontal: 2,
            vertical: 1,
        });
        let mut lines: Vec<Line> = Vec::new();
        let subject_clipped: String = modal.subject.chars().take(70).collect();
        lines.push(Line::from(vec![
            Span::raw(format!("#{} ", modal.ticket_id)).bold(),
            Span::raw(subject_clipped),
        ]));
        if let Some(org) = &modal.org {
            lines.push(Line::from(format!("Org: {org}").dim().to_string()));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(
            "Could not auto-resolve site. Enter site_name (e.g. us-ga-roswell):"
                .yellow()
                .to_string(),
        ));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::raw("site_name> ").bold(),
            Span::raw(&modal.input),
            Span::raw("_"),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(
            "Enter to submit · Esc to cancel".dim().to_string(),
        ));
        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(para, inner);
    }
}

fn handle_modal_key(modal_slot: &mut Option<SiteInputModal>, key: KeyEvent) {
    let Some(modal) = modal_slot.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Esc => {
            if let Some(tx) = modal.responder.take() {
                let _ = tx.send(None);
            }
            modal.input.clear();
        }
        KeyCode::Enter => {
            let value = modal.input.trim().to_string();
            let answer = if value.is_empty() { None } else { Some(value) };
            if let Some(tx) = modal.responder.take() {
                let _ = tx.send(answer);
            }
        }
        KeyCode::Backspace => {
            modal.input.pop();
        }
        KeyCode::Char(c) => {
            modal.input.push(c);
        }
        _ => {}
    }
}

// Because handle_key holds `&mut self.modal` and we want to also clear it on
// dismiss, we run a post-pass to drop the modal when the responder is gone.
impl InboxApp {
    fn maybe_clear_modal(&mut self) {
        if let Some(modal) = &self.modal {
            if modal.responder.is_none() {
                self.modal = None;
            }
        }
    }
}

async fn poll_iteration(
    state: State,
    opts: WatcherOptions,
    backfill_cutoff: DateTime<Utc>,
    tx: mpsc::UnboundedSender<InboxEvent>,
) -> Result<(State, HashSet<u64>), String> {
    let zd = ZendeskClient::from_env().map_err(|e| e.to_string())?;
    let view_ids: Vec<u64> = match opts.view_id {
        Some(id) => zd
            .list_view_ticket_ids(id)
            .await
            .map_err(|e| e.to_string())?,
        None => zd.list_my_ticket_ids().await.map_err(|e| e.to_string())?,
    };
    let view_set: HashSet<u64> = view_ids.iter().copied().collect();
    let mut new_state = state;

    for tid in &view_ids {
        let ticket = match zd.get_ticket(*tid).await {
            Ok(t) => t,
            Err(e) => {
                let _ = tx.send(InboxEvent::TriageFailed {
                    ticket_id: *tid,
                    error: e.to_string(),
                });
                continue;
            }
        };
        let key = tid.to_string();
        let updated = ticket.updated_at.unwrap_or(ticket.created_at);
        let needs_triage = watcher::should_triage(&ticket, &new_state, backfill_cutoff);
        if !needs_triage {
            // First-run silent backfill: mark as seen, skip note.
            new_state
                .triaged
                .entry(key.clone())
                .or_insert_with(|| updated.to_rfc3339());
            continue;
        }

        let tx_inner = tx.clone();
        let opts_inner = InvestigateOptions {
            interactive: false,
            workspace: None,
            cnc_override: None,
            site_override: None,
            anchor_override: None,
            window_minutes: opts.window_minutes,
            levels: opts.levels.clone(),
            verbose: opts.verbose,
            redact_enabled: true,
            no_llm: false,
            // Inbox auto-triage: never bypass the soft-lock.
            force: false,
        };
        let no_logs = opts.no_logs;
        let tid_copy = *tid;
        let _ = tx.send(InboxEvent::TriagePhase {
            ticket_id: tid_copy,
            label: "Triaging".into(),
            step: 1,
        });

        tokio::spawn(async move {
            match run_pipeline(ticket, opts_inner, no_logs, tx_inner.clone()).await {
                Ok(outcome) => {
                    let _ = tx_inner.send(InboxEvent::TriageComplete {
                        ticket_id: tid_copy,
                        folder: outcome.paths.folder,
                    });
                }
                Err(e) => {
                    let _ = tx_inner.send(InboxEvent::TriageFailed {
                        ticket_id: tid_copy,
                        error: e,
                    });
                }
            }
        });
        new_state.triaged.insert(key, updated.to_rfc3339());
    }
    let live_set: HashSet<String> = view_set.iter().map(|id| id.to_string()).collect();
    new_state =
        watcher::prune_by_membership(new_state, &live_set, watcher::DEFAULT_MEMBERSHIP_GRACE_DAYS);
    new_state = watcher::prune_state(
        new_state,
        watcher::DEFAULT_PRUNE_CAP,
        watcher::DEFAULT_TTL_DAYS,
        &live_set,
    );
    Ok((new_state, view_set))
}

async fn triage_one_ticket(
    ticket_id: u64,
    opts: WatcherOptions,
    tx: mpsc::UnboundedSender<InboxEvent>,
    site_override: Option<String>,
) -> Result<(), String> {
    let zd = ZendeskClient::from_env().map_err(|e| e.to_string())?;
    let _ = tx.send(InboxEvent::TriagePhase {
        ticket_id,
        label: "Fetching ticket".into(),
        step: 1,
    });
    let ticket = zd.get_ticket(ticket_id).await.map_err(|e| e.to_string())?;

    // Try resolving site. If both rules + LLM fail, ask the user.
    let sites = extract::load_site_map(Path::new("data/cnc-map.json")).unwrap_or_default();
    let effective_override = if let Some(s) = site_override.clone() {
        Some(s)
    } else {
        let (entry, _) =
            extract::lookup_site(&ticket, &sites, None, None).map_err(|e| e.to_string())?;
        if entry.is_none() && !sites.is_empty() {
            let (responder_tx, responder_rx) = oneshot::channel();
            let _ = tx.send(InboxEvent::SiteInputNeeded {
                ticket_id,
                subject: ticket.subject.clone(),
                org: ticket.requester_org.clone(),
                responder: responder_tx,
            });
            responder_rx.await.unwrap_or(None)
        } else {
            None
        }
    };

    let opts_inner = InvestigateOptions {
        site_override: effective_override,
        ..opts_to_investigate(opts.clone())
    };
    let outcome = run_pipeline(ticket, opts_inner, opts.no_logs, tx.clone()).await?;
    let _ = tx.send(InboxEvent::TriageComplete {
        ticket_id,
        folder: outcome.paths.folder,
    });
    Ok(())
}

fn opts_to_investigate(opts: WatcherOptions) -> InvestigateOptions {
    InvestigateOptions {
        interactive: false,
        workspace: None,
        cnc_override: None,
        site_override: None,
        anchor_override: None,
        window_minutes: opts.window_minutes,
        levels: opts.levels,
        verbose: opts.verbose,
        redact_enabled: true,
        no_llm: false,
        // Inbox-driven triage: never bypass the soft-lock.
        force: false,
    }
}

async fn run_pipeline(
    ticket: Ticket,
    opts: InvestigateOptions,
    no_logs: bool,
    tx: mpsc::UnboundedSender<InboxEvent>,
) -> Result<StructuredInvestigation, String> {
    let mut session = investigation::create_session(ticket.clone());
    let dd = if no_logs {
        None
    } else {
        DatadogClient::from_env().ok()
    };
    let rubric = Rubric::load().map_err(|e| e.to_string())?;
    let reporter = PhaseReporter {
        ticket_id: ticket.id,
        tx,
    };
    pipeline::investigate_one_structured(
        ticket,
        &mut session,
        dd.as_ref(),
        &rubric,
        &reporter,
        &opts,
    )
    .await
    .map_err(|e| e.to_string())
}

struct PhaseReporter {
    ticket_id: u64,
    tx: mpsc::UnboundedSender<InboxEvent>,
}

impl Reporter for PhaseReporter {
    fn phase_started(&self, phase: &str, _detail: &str) {
        let step = phase_to_step(phase);
        let _ = self.tx.send(InboxEvent::TriagePhase {
            ticket_id: self.ticket_id,
            label: pretty_phase_label(phase).to_string(),
            step,
        });
    }
    fn phase_done(&self, _phase: &str, _detail: &str) {}
    fn phase_failed(&self, _phase: &str, _err: &str) {}
}

fn phase_to_step(phase: &str) -> u8 {
    match phase {
        "customer_history" | "memory_lookup" | "evidence_intake" | "build_timeline" => 1,
        "enrichment" => 2,
        "llm_call" => 3,
        "save" => 4,
        _ => 1,
    }
}

fn pretty_phase_label(phase: &str) -> &'static str {
    match phase {
        "customer_history" => "Fetching customer history",
        "memory_lookup" => "Querying prior investigations",
        "evidence_intake" => "Reviewing evidence",
        "build_timeline" => "Building timeline",
        "enrichment" => "Querying Datadog",
        "llm_call" => "Asking LLM",
        "save" => "Writing ticket folder",
        _ => "Triaging",
    }
}

fn confidence_cell(c: &str) -> Cell<'static> {
    let normalized = c.to_ascii_lowercase();
    let (text, style) = match normalized.as_str() {
        "high" => (
            "high",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        "medium" => ("med", Style::default().fg(Color::Yellow)),
        "low" => ("low", Style::default().fg(Color::Red)),
        _ => (normalized.as_str(), Style::default()),
    };
    Cell::from(text.to_string()).style(style)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

fn relative_time(dt: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let mins = (now - dt).num_minutes();
    if mins < 2 {
        "just now".into()
    } else if mins < 60 {
        format!("{mins}m ago")
    } else if mins < 60 * 24 {
        format!("{}h ago", mins / 60)
    } else {
        format!("{}d ago", (now - dt).num_days())
    }
}

fn copy_to_clipboard(text: &str) -> bool {
    let candidates: &[&[&str]] = &[
        &["pbcopy"],
        &["wl-copy"],
        &["xclip", "-selection", "clipboard"],
    ];
    for cmd in candidates {
        let Some((bin, args)) = cmd.split_first() else {
            continue;
        };
        let mut command = std::process::Command::new(bin);
        command.args(args.iter());
        command.stdin(Stdio::piped());
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
            drop(stdin);
        }
        // We deliberately don't wait — clipboard tools either return quickly
        // or detach. Truly waiting could block xclip indefinitely (it forks
        // and holds the selection until X clears it).
        return true;
    }
    false
}

fn open_url(url: &str) -> bool {
    let bin = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    std::process::Command::new(bin)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

//
// ──────────────────────────────────────────────────────────────────────
//   Pure helpers — testable without standing up the ratatui app.
// ──────────────────────────────────────────────────────────────────────
//

/// Scan a `tickets_root` directory and return one entry per subdirectory that
/// contains a readable `STATE.md`. Entries with non-numeric folder names are
/// skipped — the spec requires `Tickets/<zendesk_id>/`.
pub fn scan_tickets_root(root: &Path) -> Vec<(u64, PathBuf, InboxStateSummary)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(id) = name.parse::<u64>() else {
            continue;
        };
        let state_path = path.join("STATE.md");
        if !state_path.is_file() {
            continue;
        }
        let summary = parse_state_md(&state_path).unwrap_or_default();
        out.push((id, path, summary));
    }
    out.sort_by_key(|(id, _, _)| *id);
    out
}

/// Parse a `STATE.md` file into the inbox summary view. Best-effort: missing
/// or malformed fields are returned as `None` / empty vectors; only an
/// unreadable file yields `None`.
pub fn parse_state_md(state_path: &Path) -> Option<InboxStateSummary> {
    let text = std::fs::read_to_string(state_path).ok()?;
    Some(parse_state_md_str(&text))
}

/// String-input variant — exposed so tests don't need a tempdir.
pub fn parse_state_md_str(text: &str) -> InboxStateSummary {
    let mut s = InboxStateSummary::default();
    let mut in_related = false;
    let mut in_validator = false;
    for line in text.lines() {
        if line.trim() == "---" {
            // YAML frontmatter delimiter — skip.
            continue;
        }

        // Track indentation: indented lines belong to the most recent
        // nested-block heading.
        let is_indented = line.starts_with([' ', '\t']);

        if !is_indented {
            in_related = false;
            in_validator = false;
        }

        // Block-list entries under `validator_warnings:` look like `  - "..."`
        // — they have no `:` so split_once below would skip them.
        if is_indented && in_validator {
            if let Some(rest) = line.trim_start().strip_prefix("- ") {
                if let Some(item) = strip_yaml_scalar(rest.trim()) {
                    s.validator_warnings.push(item);
                }
            }
            continue;
        }

        let (raw_key, raw_value) = match line.split_once(':') {
            Some(pair) => pair,
            None => continue,
        };
        let key = raw_key.trim();
        let value = raw_value.trim();

        // Top-level scalars.
        if !is_indented {
            match key {
                "ticket_id" => s.ticket_id = value.parse().ok(),
                "fork" => s.fork = strip_yaml_scalar(value),
                "confidence" => s.confidence = strip_yaml_scalar(value),
                "status" => s.status = strip_yaml_scalar(value),
                "owner" => s.owner = strip_yaml_scalar(value),
                "quoted_rubric_row" => s.quoted_rubric_row = strip_yaml_scalar(value),
                "rubric_version" => s.rubric_version = strip_yaml_scalar(value),
                "cluster" => s.cluster = strip_yaml_scalar(value),
                "updated_at" | "created_at" => {
                    let candidate = strip_yaml_scalar(value).unwrap_or_default();
                    if let Ok(parsed) = DateTime::parse_from_rfc3339(&candidate) {
                        s.updated_at = Some(parsed.with_timezone(&Utc));
                    }
                }
                "related" => {
                    in_related = true;
                }
                "validator_warnings" => {
                    // Inline list form: `validator_warnings: ["...", "..."]`
                    if value.starts_with('[') {
                        s.validator_warnings = parse_inline_str_list(value);
                    } else {
                        in_validator = true;
                    }
                }
                _ => {}
            }
            continue;
        }

        // Indented (nested) fields under `related:`.
        if in_related {
            match key.trim() {
                "zendesk" => s.related_zendesk = parse_inline_u64_list(value),
                "jira" => s.related_jira = parse_inline_str_list(value),
                "master" => {
                    let v = strip_yaml_scalar(value);
                    s.master = v.and_then(|x| x.parse().ok());
                }
                _ => {}
            }
        }
    }
    s
}

fn strip_yaml_scalar(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s == "null" || s == "~" {
        return None;
    }
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        return Some(inner.replace(r#"\""#, "\"").replace(r"\\", "\\"));
    }
    Some(s.to_string())
}

fn parse_inline_u64_list(value: &str) -> Vec<u64> {
    let v = value.trim();
    if !(v.starts_with('[') && v.ends_with(']')) {
        return Vec::new();
    }
    v[1..v.len() - 1]
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u64>().ok())
        .collect()
}

fn parse_inline_str_list(value: &str) -> Vec<String> {
    let v = value.trim();
    if !(v.starts_with('[') && v.ends_with(']')) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_string = false;
    let mut escape = false;
    for ch in v[1..v.len() - 1].chars() {
        if escape {
            buf.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            if !in_string {
                out.push(std::mem::take(&mut buf));
            }
            continue;
        }
        if in_string {
            buf.push(ch);
        }
    }
    out
}

/// Render the synth single-pane summary as a list of lines. The shipped
/// rubric version is included so a mismatch is obvious side-by-side.
pub fn render_synth_summary(
    ticket_id: u64,
    state: &InboxStateSummary,
    shipped_rubric_version: &str,
) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("Ticket: ZD-{ticket_id}"));
    lines.push(format!(
        "Fork:   {} · Confidence: {} · Status: {}",
        state.fork.clone().unwrap_or_else(|| "—".into()),
        state.confidence.clone().unwrap_or_else(|| "—".into()),
        state.status.clone().unwrap_or_else(|| "open".into()),
    ));
    lines.push(format!(
        "Owner:  {}",
        state.owner.clone().unwrap_or_else(|| "(unowned)".into())
    ));
    lines.push(String::new());

    lines.push("Quoted rubric row:".to_string());
    let row = state
        .quoted_rubric_row
        .clone()
        .unwrap_or_else(|| "(none)".into());
    lines.push(format!("  \"{}\"", row));
    lines.push(format!(
        "  rubric_version on STATE.md: {}",
        state
            .rubric_version
            .clone()
            .unwrap_or_else(|| "(unset)".into())
    ));
    lines.push(format!(
        "  shipped rubric_version:    {shipped_rubric_version}"
    ));
    lines.push(String::new());

    lines.push("Related:".to_string());
    let zd = if state.related_zendesk.is_empty() {
        "(none)".to_string()
    } else {
        state
            .related_zendesk
            .iter()
            .map(|i| format!("#{i}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    lines.push(format!("  Zendesk: {zd}"));
    let jira = if state.related_jira.is_empty() {
        "(none)".to_string()
    } else {
        state.related_jira.join(", ")
    };
    lines.push(format!("  Jira:    {jira}"));
    lines.push(format!(
        "  Master:  {}",
        state
            .master
            .map(|i| format!("#{i}"))
            .unwrap_or_else(|| "(none)".into()),
    ));
    if let Some(c) = &state.cluster {
        lines.push(format!("  Cluster: {c}"));
    }

    if !state.validator_warnings.is_empty() {
        lines.push(String::new());
        lines.push("Validator soft-warnings (accepted):".to_string());
        for w in &state.validator_warnings {
            lines.push(format!("  · {w}"));
        }
    }
    lines
}

/// Render the rubric-version mismatch banner line.
pub fn rubric_mismatch_banner(state_version: &str, shipped_version: &str) -> String {
    format!("⚠ Rubric version mismatch: state={state_version}, shipped={shipped_version}")
}

/// Returns true when the on-disk `STATE.md` `rubric_version` does not match
/// the shipped rubric's version. Missing `rubric_version` on the artifact is
/// treated as "no mismatch detected" (we have nothing to compare against).
pub fn rubric_mismatch(state: &InboxStateSummary, shipped_rubric_version: &str) -> bool {
    state
        .rubric_version
        .as_deref()
        .is_some_and(|v| v != shipped_rubric_version)
}

/// Read a single ticket-folder file into a displayable string. On failure
/// returns a clear placeholder so the UI never panics on a missing file.
fn read_file_for_display(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => format!(
            "(could not read {}: {e})",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("file"),
        ),
    }
}

// Suppress unused-warning when the only `ticket_folder` symbol used here is
// `tickets_root()`. The import is kept explicit so the dependency is obvious.
#[allow(dead_code)]
fn _ticket_folder_anchor() -> std::path::PathBuf {
    ticket_folder::tickets_root()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_state_md() -> &'static str {
        r#"---
ticket_id: 44671
fork: B
confidence: medium
quoted_rubric_row: "customer LAN, switch, or SDWAN. Link to site master ticket"
rubric_version: "2026-04-30"
owner: "alice@example.com"
created_at: 2026-05-13T07:32:11Z
updated_at: 2026-05-13T07:32:11Z
status: open
related:
  zendesk: [43874, 42708]
  jira: ["REP-1234", "REP-5678"]
  master: null
cluster: "jeffcom-network-error"
validator_warnings: ["quoted_rubric_row paraphrased"]
---
"#
    }

    #[test]
    fn parse_state_md_extracts_top_level_scalars() {
        let s = parse_state_md_str(sample_state_md());
        assert_eq!(s.ticket_id, Some(44671));
        assert_eq!(s.fork.as_deref(), Some("B"));
        assert_eq!(s.confidence.as_deref(), Some("medium"));
        assert_eq!(s.status.as_deref(), Some("open"));
        assert_eq!(s.owner.as_deref(), Some("alice@example.com"));
        assert_eq!(
            s.quoted_rubric_row.as_deref(),
            Some("customer LAN, switch, or SDWAN. Link to site master ticket")
        );
        assert_eq!(s.rubric_version.as_deref(), Some("2026-04-30"));
        assert_eq!(s.cluster.as_deref(), Some("jeffcom-network-error"));
    }

    #[test]
    fn parse_state_md_parses_related_lists() {
        let s = parse_state_md_str(sample_state_md());
        assert_eq!(s.related_zendesk, vec![43874, 42708]);
        assert_eq!(s.related_jira, vec!["REP-1234", "REP-5678"]);
        assert!(s.master.is_none());
    }

    #[test]
    fn parse_state_md_parses_validator_warnings_inline() {
        let s = parse_state_md_str(sample_state_md());
        assert_eq!(
            s.validator_warnings,
            vec!["quoted_rubric_row paraphrased".to_string()]
        );
    }

    #[test]
    fn parse_state_md_parses_validator_warnings_block_form() {
        // Block-list form is what a YAML pretty-printer might emit. We accept
        // either inline or block.
        let text = r#"---
ticket_id: 1
fork: A
validator_warnings:
  - "first warning"
  - "second warning"
---
"#;
        let s = parse_state_md_str(text);
        assert_eq!(
            s.validator_warnings,
            vec!["first warning".to_string(), "second warning".to_string()]
        );
    }

    #[test]
    fn parse_state_md_handles_missing_optional_fields() {
        let text = r#"---
ticket_id: 12345
fork: D
confidence: low
status: open
---
"#;
        let s = parse_state_md_str(text);
        assert_eq!(s.ticket_id, Some(12345));
        assert_eq!(s.fork.as_deref(), Some("D"));
        assert!(s.owner.is_none());
        assert!(s.quoted_rubric_row.is_none());
        assert!(s.related_zendesk.is_empty());
        assert!(s.master.is_none());
    }

    #[test]
    fn parse_state_md_treats_null_as_none() {
        let text = "---\nticket_id: 1\nfork: A\ncluster: null\n---\n";
        let s = parse_state_md_str(text);
        assert!(s.cluster.is_none());
    }

    #[test]
    fn synth_summary_contains_fork_confidence_rubric_owner_status_and_related() {
        let s = parse_state_md_str(sample_state_md());
        let out = render_synth_summary(44671, &s, "2026-05-13");
        let joined = out.join("\n");
        assert!(joined.contains("ZD-44671"));
        assert!(joined.contains("Fork:   B"));
        assert!(joined.contains("Confidence: medium"));
        assert!(joined.contains("Status: open"));
        assert!(joined.contains("alice@example.com"));
        assert!(joined.contains("customer LAN, switch, or SDWAN"));
        assert!(joined.contains("#43874"));
        assert!(joined.contains("#42708"));
        assert!(joined.contains("REP-1234"));
    }

    #[test]
    fn synth_summary_surfaces_both_rubric_versions() {
        // Mismatch must be obvious in the summary even without the banner.
        let s = parse_state_md_str(sample_state_md());
        let out = render_synth_summary(44671, &s, "2026-05-13").join("\n");
        assert!(out.contains("STATE.md: 2026-04-30"));
        assert!(out.contains("shipped rubric_version:    2026-05-13"));
    }

    #[test]
    fn synth_summary_handles_no_related() {
        let text = "---\nticket_id: 1\nfork: A\nconfidence: high\nowner: x@y\nstatus: open\n---\n";
        let s = parse_state_md_str(text);
        let out = render_synth_summary(1, &s, "2026-05-13").join("\n");
        assert!(out.contains("Zendesk: (none)"));
        assert!(out.contains("Jira:    (none)"));
        assert!(out.contains("Master:  (none)"));
    }

    #[test]
    fn rubric_mismatch_detects_drift() {
        let s = parse_state_md_str(sample_state_md());
        assert!(rubric_mismatch(&s, "2026-05-13"));
    }

    #[test]
    fn rubric_mismatch_quiet_on_match() {
        let s = parse_state_md_str(sample_state_md());
        assert!(!rubric_mismatch(&s, "2026-04-30"));
    }

    #[test]
    fn rubric_mismatch_quiet_when_version_unset() {
        let s = InboxStateSummary {
            rubric_version: None,
            ..Default::default()
        };
        assert!(!rubric_mismatch(&s, "2026-05-13"));
    }

    #[test]
    fn rubric_mismatch_banner_names_both_versions() {
        let line = rubric_mismatch_banner("2026-04-30", "2026-05-13");
        assert!(line.contains("2026-04-30"));
        assert!(line.contains("2026-05-13"));
        assert!(line.contains("Rubric version mismatch"));
    }

    #[test]
    fn scan_tickets_root_returns_only_dirs_with_state_md() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Valid ticket folder.
        let ok = root.join("44671");
        std::fs::create_dir_all(&ok).unwrap();
        std::fs::write(ok.join("STATE.md"), sample_state_md()).unwrap();

        // Missing STATE.md → must be skipped.
        let no_state = root.join("99999");
        std::fs::create_dir_all(&no_state).unwrap();
        std::fs::write(no_state.join("INTAKE.md"), "x").unwrap();

        // Non-numeric folder → must be skipped.
        let weird = root.join("not-a-ticket");
        std::fs::create_dir_all(&weird).unwrap();
        std::fs::write(weird.join("STATE.md"), sample_state_md()).unwrap();

        // Stray file in root → must be skipped.
        std::fs::write(root.join("stray.md"), "x").unwrap();

        let entries = scan_tickets_root(root);
        let ids: Vec<u64> = entries.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids, vec![44671], "got entries: {entries:?}");
    }

    #[test]
    fn scan_tickets_root_parses_state_into_summary() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let folder = root.join("12345");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("STATE.md"), sample_state_md()).unwrap();

        let entries = scan_tickets_root(root);
        assert_eq!(entries.len(), 1);
        let (id, path, summary) = &entries[0];
        assert_eq!(*id, 12345);
        assert_eq!(path, &folder);
        assert_eq!(summary.fork.as_deref(), Some("B"));
        assert_eq!(
            summary.quoted_rubric_row.as_deref(),
            Some("customer LAN, switch, or SDWAN. Link to site master ticket")
        );
    }
}
