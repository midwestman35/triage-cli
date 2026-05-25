use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::poll::{poll_iteration, triage_one_ticket};
use super::render;
use super::state::{parse_state_md, scan_tickets_root, InboxStateSummary};
use super::TABS;
use crate::playbook::Rubric;
use crate::tui::effects::InboxEffects;
use crate::watcher::{self, State, WatcherOptions};

const NOTIFY_TTL: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Triaged,
    Triaging,
    Queued,
    Failed,
}

impl Status {
    pub(crate) fn priority(&self) -> u8 {
        match self {
            Status::Triaging => 0,
            Status::Triaged => 1,
            Status::Queued => 2,
            Status::Failed => 3,
        }
    }

    pub(crate) fn icon(&self) -> char {
        match self {
            Status::Triaged => '✓',
            Status::Triaging => '→',
            Status::Queued => '○',
            Status::Failed => '✗',
        }
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Status::Triaged => "triaged",
            Status::Triaging => "triaging…",
            Status::Queued => "in queue",
            Status::Failed => "failed",
        }
    }

    pub(crate) fn row_bg(&self) -> Option<Color> {
        match self {
            Status::Failed => Some(Color::Rgb(60, 0, 0)),
            Status::Triaging => Some(Color::Rgb(60, 40, 0)),
            _ => None,
        }
    }
}

/// One on-disk ticket folder. Read lazily; the only field eagerly populated is
/// `state` so the row list can render summary columns.
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
pub(crate) enum Focus {
    List,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetailMode {
    Summary,
    File(usize),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum NotifyKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub(crate) struct Notification {
    pub(crate) kind: NotifyKind,
    pub(crate) text: String,
    pub(crate) expires_at: Instant,
}

#[derive(Debug)]
pub(crate) enum InboxEvent {
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
        updated_at: DateTime<Utc>,
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

pub(crate) struct SiteInputModal {
    pub(crate) ticket_id: u64,
    pub(crate) subject: String,
    pub(crate) org: Option<String>,
    pub(crate) input: String,
    pub(crate) responder: Option<oneshot::Sender<Option<String>>>,
}

pub(crate) struct InboxApp {
    pub(crate) opts: WatcherOptions,
    pub(crate) tickets_root: PathBuf,
    pub(crate) rubric: Rubric,
    pub(crate) rows: BTreeMap<u64, RowEntry>,
    state: State,
    backfill_cutoff: DateTime<Utc>,
    view_ids: HashSet<u64>,
    in_flight_triages: HashSet<u64>,
    poll_success_overrides: BTreeMap<String, String>,
    pub(crate) cursor: usize,
    pub(crate) focus: Focus,
    pub(crate) detail_mode: DetailMode,
    pub(crate) polling: bool,
    pub(crate) last_poll: Option<DateTime<Utc>>,
    pub(crate) notification: Option<Notification>,
    pub(crate) effects: InboxEffects,
    last_frame: Instant,
    pub(crate) report_scroll: u16,
    pub(crate) modal: Option<SiteInputModal>,
    pending_triages: Vec<JoinHandle<()>>,
    event_tx: mpsc::UnboundedSender<InboxEvent>,
    should_exit: bool,
    pending_chat_ticket_id: Option<String>,
}

impl InboxApp {
    pub(crate) fn new(
        opts: WatcherOptions,
        tickets_root: PathBuf,
        rubric: Rubric,
        state: State,
        backfill_cutoff: DateTime<Utc>,
        event_tx: mpsc::UnboundedSender<InboxEvent>,
    ) -> Self {
        Self {
            opts,
            tickets_root,
            rubric,
            rows: BTreeMap::new(),
            state,
            backfill_cutoff,
            view_ids: HashSet::new(),
            in_flight_triages: HashSet::new(),
            poll_success_overrides: BTreeMap::new(),
            cursor: 0,
            focus: Focus::List,
            detail_mode: DetailMode::Summary,
            polling: false,
            last_poll: None,
            notification: None,
            effects: InboxEffects::disabled(),
            last_frame: Instant::now(),
            report_scroll: 0,
            modal: None,
            pending_triages: Vec::new(),
            event_tx,
            should_exit: false,
            pending_chat_ticket_id: None,
        }
    }

    pub(crate) fn hydrate_from_disk(&mut self) {
        for (id, folder, summary) in scan_tickets_root(&self.tickets_root) {
            self.rows.insert(id, triaged_row(id, folder, summary));
        }
    }

    pub(crate) fn sorted_rows(&self) -> Vec<RowEntry> {
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

    pub(crate) fn notify(&mut self, text: impl Into<String>, kind: NotifyKind) {
        self.notification = Some(Notification {
            kind,
            text: text.into(),
            expires_at: Instant::now() + NOTIFY_TTL,
        });
    }

    fn merge_poll_state(&self, mut polled_state: State) -> State {
        for (ticket_id, updated_at) in &self.poll_success_overrides {
            polled_state
                .triaged
                .insert(ticket_id.clone(), updated_at.clone());
        }
        polled_state
    }

    pub(crate) fn spawn_poll(&mut self) {
        if self.polling || self.modal.is_some() {
            return;
        }
        self.polling = true;
        self.poll_success_overrides.clear();
        let tx = self.event_tx.clone();
        let opts = self.opts.clone();
        let backfill_cutoff = self.backfill_cutoff;
        let in_flight_triages = self.in_flight_triages.clone();
        let state_snapshot = State {
            version: self.state.version,
            triaged: self.state.triaged.clone(),
        };
        let _ = tx.send(InboxEvent::PollStarted);
        let handle = tokio::spawn(async move {
            match poll_iteration(
                state_snapshot,
                opts,
                backfill_cutoff,
                in_flight_triages,
                tx.clone(),
            )
            .await
            {
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

    pub(crate) fn reload_row_from_disk(&mut self, ticket_id: u64, folder: PathBuf) {
        let state_path = folder.join("STATE.md");
        let summary = parse_state_md(&state_path).unwrap_or_default();
        self.rows
            .insert(ticket_id, triaged_row(ticket_id, folder, summary));
    }

    pub(crate) fn handle_event(&mut self, ev: InboxEvent) {
        match ev {
            InboxEvent::PollStarted => {
                self.polling = true;
                self.poll_success_overrides.clear();
            }
            InboxEvent::PollFinished {
                new_state,
                view_ids,
            } => {
                self.polling = false;
                self.state = self.merge_poll_state(new_state);
                self.poll_success_overrides.clear();
                let _ = watcher::save_state(&self.opts.state_file, &self.state);
                self.view_ids = view_ids;
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
                self.poll_success_overrides.clear();
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
                self.in_flight_triages.insert(ticket_id);
                if let Some(entry) = self.rows.get_mut(&ticket_id) {
                    entry.status = Status::Triaging;
                    entry.phase_label = Some(label);
                    entry.phase_step = step;
                }
            }
            InboxEvent::TriageComplete {
                ticket_id,
                folder,
                updated_at,
            } => {
                self.in_flight_triages.remove(&ticket_id);
                if self.polling {
                    self.poll_success_overrides
                        .insert(ticket_id.to_string(), updated_at.to_rfc3339());
                }
                self.state
                    .triaged
                    .insert(ticket_id.to_string(), updated_at.to_rfc3339());
                let _ = watcher::save_state(&self.opts.state_file, &self.state);
                self.reload_row_from_disk(ticket_id, folder);
            }
            InboxEvent::TriageFailed { ticket_id, error } => {
                self.in_flight_triages.remove(&ticket_id);
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

    pub(crate) fn handle_key(&mut self, key: KeyEvent) {
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
            (KeyCode::Char('a'), _) => self.action_open_chat(),
            (KeyCode::PageDown, _) => self.report_scroll = self.report_scroll.saturating_add(8),
            (KeyCode::PageUp, _) => self.report_scroll = self.report_scroll.saturating_sub(8),
            _ => {}
        }
    }

    pub(crate) fn maybe_clear_modal(&mut self) {
        if let Some(modal) = &self.modal {
            if modal.responder.is_none() {
                self.modal = None;
            }
        }
    }

    pub(crate) fn selected_row(&self) -> Option<RowEntry> {
        self.sorted_rows().into_iter().nth(self.cursor)
    }

    pub(crate) fn draw(&mut self, frame: &mut ratatui::Frame) {
        render::draw(self, frame);
    }

    pub(crate) fn expire_notifications(&mut self, now: Instant) {
        if let Some(n) = &self.notification {
            if n.expires_at <= now {
                self.notification = None;
            }
        }
    }

    pub(crate) fn tick_effects(&mut self, elapsed: Duration) {
        self.effects.tick(elapsed);
    }

    pub(crate) fn wants_animation_frame(&self) -> bool {
        self.effects.wants_animation_frame()
    }

    pub(crate) fn last_frame(&self) -> Instant {
        self.last_frame
    }

    pub(crate) fn set_last_frame(&mut self, instant: Instant) {
        self.last_frame = instant;
    }

    pub(crate) fn should_exit(&self) -> bool {
        self.should_exit
    }

    pub(crate) fn take_pending_chat_ticket_id(&mut self) -> Option<String> {
        self.pending_chat_ticket_id.take()
    }

    pub(crate) fn state(&self) -> &State {
        &self.state
    }

    pub(crate) fn abort_pending_triages(&mut self) {
        for handle in self.pending_triages.drain(..) {
            handle.abort();
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
            _ => self.focus = Focus::Detail,
        }
    }

    fn action_cycle_tab(&mut self) {
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
        let text =
            render::render_synth_summary(row.ticket_id, state, self.rubric.version()).join("\n");
        if copy_to_clipboard(&text) {
            self.notify("Copied synth summary", NotifyKind::Success);
        } else {
            self.notify(
                "Clipboard not available on this system",
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

    fn action_open_chat(&mut self) {
        let Some(row) = self.selected_row() else {
            self.notify("No ticket selected", NotifyKind::Warning);
            return;
        };
        self.pending_chat_ticket_id = Some(row.ticket_id.to_string());
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
}

fn triaged_row(ticket_id: u64, folder: PathBuf, summary: InboxStateSummary) -> RowEntry {
    RowEntry {
        ticket_id,
        status: Status::Triaged,
        folder: Some(folder),
        state: Some(summary),
        site_hint: None,
        failure_reason: None,
        phase_label: None,
        phase_step: 0,
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

fn copy_to_clipboard(text: &str) -> bool {
    use arboard::Clipboard;
    Clipboard::new()
        .and_then(|mut c| c.set_text(text.to_owned()))
        .is_ok()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn sample_watcher_opts(state_file: &Path) -> WatcherOptions {
        WatcherOptions {
            view_id: Some(44),
            interval: 30,
            state_file: state_file.to_path_buf(),
            backfill_hours: 24.0,
            window_minutes: 15,
            levels: vec!["error".into()],
            no_logs: true,
            print_notes: false,
            verbose: false,
        }
    }

    fn write_ticket_state(folder: &Path, ticket_id: u64, updated_at: &str) {
        let body = format!(
            r#"---
ticket_id: {ticket_id}
fork: B
confidence: medium
quoted_rubric_row: "row"
rubric_version: "2026-04-30"
owner: "alice@example.com"
created_at: {updated_at}
updated_at: {updated_at}
status: open
related:
  zendesk: []
  jira: []
  master: null
cluster: null
validator_warnings: []
---
"#
        );
        std::fs::create_dir_all(folder).unwrap();
        std::fs::write(folder.join("STATE.md"), body).unwrap();
    }

    fn make_test_inbox_app(state_file: &Path, tickets_root: &Path) -> InboxApp {
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        InboxApp::new(
            sample_watcher_opts(state_file),
            tickets_root.to_path_buf(),
            Rubric::load().unwrap(),
            State {
                version: 1,
                triaged: BTreeMap::new(),
            },
            DateTime::parse_from_rfc3339("2026-05-25T11:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            event_tx,
        )
    }

    #[test]
    fn poll_finished_preserves_success_recorded_before_snapshot_arrives() {
        let tmp = tempfile::tempdir().unwrap();
        let state_file = tmp.path().join("watcher-state.json");
        let ticket_id = 44671;
        let updated_at = "2026-05-25T12:00:00Z";
        let folder = tmp.path().join(ticket_id.to_string());
        write_ticket_state(&folder, ticket_id, updated_at);

        let mut app = make_test_inbox_app(&state_file, tmp.path());
        app.handle_event(InboxEvent::PollStarted);
        app.handle_event(InboxEvent::TriageComplete {
            ticket_id,
            folder: folder.clone(),
            updated_at: DateTime::parse_from_rfc3339(updated_at)
                .unwrap()
                .with_timezone(&Utc),
        });
        app.handle_event(InboxEvent::PollFinished {
            new_state: State {
                version: 1,
                triaged: BTreeMap::new(),
            },
            view_ids: HashSet::from([ticket_id]),
        });

        let stored = DateTime::parse_from_rfc3339(
            app.state
                .triaged
                .get(&ticket_id.to_string())
                .expect("success timestamp should remain recorded"),
        )
        .unwrap()
        .with_timezone(&Utc);
        assert_eq!(
            stored,
            DateTime::parse_from_rfc3339(updated_at)
                .unwrap()
                .with_timezone(&Utc),
            "PollFinished must not clobber a success timestamp already applied by TriageComplete"
        );
        assert!(app.poll_success_overrides.is_empty());
    }
}
