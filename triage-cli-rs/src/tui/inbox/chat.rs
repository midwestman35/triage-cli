use std::io;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::datadog::{DatadogClient, DatadogSource};
use crate::investigation;
use crate::pipeline;
use crate::ticket_folder;

fn open_required_chat_logger(ticket_dir: &Path) -> anyhow::Result<crate::chat::ChatLogger> {
    crate::chat::ChatLogger::open(ticket_dir).map_err(|e| {
        anyhow::anyhow!(
            "could not open chat event logger at {}: {e}",
            crate::chat::chat_events_log_path(ticket_dir).display()
        )
    })
}

fn global_chat_close_reason(key: KeyEvent) -> Option<crate::chat::SessionCloseReason> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(crate::chat::SessionCloseReason::CtrlC),
        _ => None,
    }
}

struct RawModeGuard;

struct ActiveChatJob {
    handle: tokio::task::JoinHandle<Result<(), String>>,
    cancel_tx: Option<tokio::sync::watch::Sender<Option<crate::providers::FollowupCancel>>>,
}

impl RawModeGuard {
    fn enter() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

// Suppress unused-warning when the only `ticket_folder` symbol used here is
// `tickets_root()`. The import is kept explicit so the dependency is obvious.
#[allow(dead_code)]
fn _ticket_folder_anchor() -> std::path::PathBuf {
    ticket_folder::tickets_root()
}

// ──────────────────────────────────────────────────────────────────────
//   Chat pane event loop — invoked by the 'a' keybinding
// ──────────────────────────────────────────────────────────────────────

pub(crate) async fn run_chat_session(ticket_id: &str) -> anyhow::Result<()> {
    use crate::providers::get_provider;
    use crate::tui::chat::{parse_chat_command, ChatCommand, ChatInputSurface};
    use crate::{chat, pipeline};
    use crossterm::event::{self, Event, KeyCode, KeyModifiers};
    use ratatui::backend::CrosstermBackend;
    use ratatui::Terminal;
    use std::sync::Arc;
    use std::time::Duration;
    use tui_textarea::TextArea;

    enum ChatInputMode {
        Ask,
        FilePath(String),
        PasteLine(String),
        DirPath(String),
    }

    let ticket_dir = ticket_folder::tickets_root().join(ticket_id);
    std::fs::create_dir_all(&ticket_dir)?;
    let (chat_tx, mut chat_rx) = tokio::sync::mpsc::unbounded_channel::<chat::ChatEvent>();
    let mut chat_logger = open_required_chat_logger(&ticket_dir)?;
    let _ = chat_tx.send(chat::ChatEvent::SessionOpened {
        ticket_id: ticket_id.to_string(),
        ts: chrono::Utc::now(),
    });

    let provider: Arc<dyn crate::providers::LlmProvider> = match get_provider() {
        Ok(provider) => Arc::from(provider),
        Err(e) => {
            let _ = chat_tx.send(chat::ChatEvent::SessionClosed {
                ts: chrono::Utc::now(),
                reason: chat::SessionCloseReason::ProviderUnavailable,
            });
            while let Ok(evt) = chat_rx.try_recv() {
                chat_logger.log(&evt);
            }
            return Err(anyhow::anyhow!("provider unavailable: {e}"));
        }
    };

    // Create a fresh terminal using stderr so we don't conflict with the
    // stdout-based inbox terminal that was suspended by the caller.
    let _raw_mode = RawModeGuard::enter()?;
    let stderr = std::io::stderr();
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;

    let mut ask_input = TextArea::default();
    ask_input.set_block(ratatui::widgets::Block::default());
    let mut input_mode = ChatInputMode::Ask;

    let mut pending_evidence: Vec<crate::models::EvidenceProvenance> = Vec::new();
    let mut in_flight: Option<chat::ChatProgress> = None;
    let mut status_hint: Option<String> = None;
    let mut active_job: Option<ActiveChatJob> = None;
    let mut pending_close_after_cancel: Option<chat::SessionCloseReason> = None;
    let mut turn_started: Option<std::time::Instant> = None;
    let transcript_scroll = 0;
    let mut transcript_follow_bottom = true;
    let close_reason = loop {
        drain_chat_events(
            &mut chat_rx,
            &mut chat_logger,
            &mut active_job,
            &mut in_flight,
            &mut turn_started,
            &mut status_hint,
            &mut ask_input,
            &mut transcript_follow_bottom,
            &ticket_dir,
            ticket_id,
        );

        if let Some(job) = active_job.as_ref() {
            if job.handle.is_finished() {
                let finished = active_job.take().expect("just checked");
                match finished.handle.await {
                    Ok(Ok(())) => {
                        status_hint = None;
                        clear_textarea(&mut ask_input);
                        transcript_follow_bottom = true;
                    }
                    Ok(Err(msg)) => {
                        drain_chat_events(
                            &mut chat_rx,
                            &mut chat_logger,
                            &mut active_job,
                            &mut in_flight,
                            &mut turn_started,
                            &mut status_hint,
                            &mut ask_input,
                            &mut transcript_follow_bottom,
                            &ticket_dir,
                            ticket_id,
                        );
                        if status_hint.as_deref() != Some(msg.as_str()) {
                            let _ = append_chat_system_turn(
                                &ticket_dir,
                                ticket_id,
                                &format!("follow-up failed: {msg}"),
                            );
                        }
                        status_hint = Some(msg);
                    }
                    Err(e) => {
                        let msg = format!("chat task panicked: {e}");
                        let _ = append_chat_system_turn(&ticket_dir, ticket_id, &msg);
                        status_hint = Some(msg);
                    }
                }
                turn_started = None;
            } else if let Some(started) = turn_started {
                let elapsed = started.elapsed().as_secs_f64();
                in_flight = in_flight.map(|p| chat::advance_progress_tick(p, elapsed));
            }
        }
        if active_job.is_none() {
            if let Some(reason) = pending_close_after_cancel.take() {
                break reason;
            }
        }

        let outcome = chat::parse_conversation_jsonl(&chat::conversation_jsonl_path(&ticket_dir))?;
        let input_surface = match &input_mode {
            ChatInputMode::Ask => ChatInputSurface::Ask(&ask_input),
            ChatInputMode::FilePath(value) => ChatInputSurface::FilePath { value },
            ChatInputMode::PasteLine(value) => ChatInputSurface::PasteLine { value },
            ChatInputMode::DirPath(value) => ChatInputSurface::DirPath { value },
        };
        let pane = crate::tui::chat::ChatPane {
            turns: &outcome.turns,
            input: input_surface,
            ticket_id,
            progress: in_flight.as_ref(),
            status_hint: status_hint.as_deref(),
            transcript_scroll,
            transcript_follow_bottom,
        };
        terminal.draw(|f| {
            let area = f.area();
            f.render_widget(&pane, area);
        })?;

        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = event::read()? {
                if let Some(reason) = global_chat_close_reason(key) {
                    if active_job.is_some() {
                        if request_active_job_cancel(active_job.as_ref(), chat::CancelSource::CtrlC)
                        {
                            pending_close_after_cancel = Some(reason);
                            status_hint = Some("interrupt requested".into());
                            continue;
                        }
                        if let Some(job) = active_job.take() {
                            job.handle.abort();
                        }
                        let _ = chat_tx.send(chat::ChatEvent::Cancelled {
                            ts: chrono::Utc::now(),
                            by: chat::CancelSource::CtrlC,
                        });
                    }
                    break reason;
                }

                if active_job.is_some() {
                    if let (KeyCode::Esc, _) = (key.code, key.modifiers) {
                        if request_active_job_cancel(
                            active_job.as_ref(),
                            chat::CancelSource::EscKey,
                        ) {
                            status_hint = Some("interrupt requested".into());
                        } else if let Some(job) = active_job.take() {
                            job.handle.abort();
                            let _ = chat_tx.send(chat::ChatEvent::Cancelled {
                                ts: chrono::Utc::now(),
                                by: chat::CancelSource::EscKey,
                            });
                            in_flight = None;
                            turn_started = None;
                            status_hint = Some("turn cancelled".into());
                        }
                    }
                    continue;
                }

                match &mut input_mode {
                    ChatInputMode::Ask => match (key.code, key.modifiers) {
                        (KeyCode::Esc, _) => {
                            break chat::SessionCloseReason::EscFromAsk;
                        }
                        (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                            let _ = chat_tx.send(chat::ChatEvent::KeyCommand {
                                ts: chrono::Utc::now(),
                                command: "/file".into(),
                            });
                            input_mode = ChatInputMode::FilePath(String::new());
                        }
                        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                            let _ = chat_tx.send(chat::ChatEvent::KeyCommand {
                                ts: chrono::Utc::now(),
                                command: "/dir".into(),
                            });
                            input_mode = ChatInputMode::DirPath(String::new());
                        }
                        (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                            let _ = chat_tx.send(chat::ChatEvent::KeyCommand {
                                ts: chrono::Utc::now(),
                                command: "/paste".into(),
                            });
                            input_mode = ChatInputMode::PasteLine(String::new());
                        }
                        (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
                            let retry_outcome = chat::parse_conversation_jsonl(
                                &chat::conversation_jsonl_path(&ticket_dir),
                            )?;
                            if let Some((body, evidence)) =
                                latest_analyst_retry_payload(&retry_outcome.turns)
                            {
                                let _ = chat_tx.send(chat::ChatEvent::KeyCommand {
                                    ts: chrono::Utc::now(),
                                    command: "/retry".into(),
                                });
                                let td = ticket_dir.clone();
                                let tid = ticket_id.to_string();
                                let provider = provider.clone();
                                let tx = chat_tx.clone();
                                begin_analyst_turn(&mut in_flight, &mut turn_started);
                                active_job =
                                    Some(spawn_analyst_job(td, tid, body, evidence, provider, tx));
                            }
                        }
                        (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                            let _ = chat_tx.send(chat::ChatEvent::KeyCommand {
                                ts: chrono::Utc::now(),
                                command: "/revise".into(),
                            });
                            let dd = DatadogClient::from_env().ok();
                            let dd_source: Option<&dyn DatadogSource> =
                                dd.as_ref().map(|d| d as &dyn DatadogSource);
                            pipeline::revise(
                                &ticket_dir,
                                ticket_id,
                                None,
                                dd_source,
                                &pipeline::InvestigateOptions::defaults(),
                            )
                            .await?;
                            clear_textarea(&mut ask_input);
                        }
                        (KeyCode::Char('s'), KeyModifiers::CONTROL) => {
                            let body: String = ask_input.lines().join("\n");
                            if body.trim().is_empty() {
                                continue;
                            }
                            let cmd = parse_chat_command(&body);
                            let _ = chat_tx.send(chat::ChatEvent::KeyCommand {
                                ts: chrono::Utc::now(),
                                command: command_label(&cmd).to_string(),
                            });
                            match cmd {
                                ChatCommand::Body(b) => {
                                    let td = ticket_dir.clone();
                                    let tid = ticket_id.to_string();
                                    let evidence = std::mem::take(&mut pending_evidence);
                                    let provider = provider.clone();
                                    let tx = chat_tx.clone();
                                    begin_analyst_turn(&mut in_flight, &mut turn_started);
                                    active_job =
                                        Some(spawn_analyst_job(td, tid, b, evidence, provider, tx));
                                }
                                ChatCommand::File(path) => {
                                    attach_file_to_pending(
                                        &ticket_dir,
                                        ticket_id,
                                        &mut pending_evidence,
                                        &chat_tx,
                                        &path,
                                    )?;
                                    clear_textarea(&mut ask_input);
                                }
                                ChatCommand::Dir {
                                    path,
                                    recursive,
                                    glob,
                                } => {
                                    attach_dir_to_pending(
                                        &ticket_dir,
                                        ticket_id,
                                        &mut pending_evidence,
                                        &chat_tx,
                                        &path,
                                        recursive,
                                        glob.as_deref(),
                                    )?;
                                    clear_textarea(&mut ask_input);
                                }
                                ChatCommand::Paste { label, body } => {
                                    let prov = chat::attach_paste(&label, &body);
                                    let _ = chat_tx.send(chat::ChatEvent::EvidenceAttached {
                                        ts: chrono::Utc::now(),
                                        provenance: prov.clone(),
                                    });
                                    pending_evidence.push(prov);
                                    clear_textarea(&mut ask_input);
                                }
                                ChatCommand::Revise => {
                                    // Construct a Datadog client per-revise so the
                                    // structured pipeline can re-fetch logs around
                                    // the original anchor. `None` is fine when the
                                    // env isn't configured — the pipeline degrades
                                    // gracefully and leans on the base-evidence
                                    // catalog plus any newly attached evidence.
                                    let dd = DatadogClient::from_env().ok();
                                    let dd_source: Option<&dyn DatadogSource> =
                                        dd.as_ref().map(|d| d as &dyn DatadogSource);
                                    pipeline::revise(
                                        &ticket_dir,
                                        ticket_id,
                                        None,
                                        dd_source,
                                        &pipeline::InvestigateOptions::defaults(),
                                    )
                                    .await?;
                                    clear_textarea(&mut ask_input);
                                }
                                ChatCommand::Retry => {
                                    let retry_outcome = chat::parse_conversation_jsonl(
                                        &chat::conversation_jsonl_path(&ticket_dir),
                                    )?;
                                    if let Some((body, evidence)) =
                                        latest_analyst_retry_payload(&retry_outcome.turns)
                                    {
                                        let td = ticket_dir.clone();
                                        let tid = ticket_id.to_string();
                                        let provider = provider.clone();
                                        let tx = chat_tx.clone();
                                        begin_analyst_turn(&mut in_flight, &mut turn_started);
                                        active_job = Some(spawn_analyst_job(
                                            td, tid, body, evidence, provider, tx,
                                        ));
                                    }
                                }
                                ChatCommand::Quit => {
                                    break chat::SessionCloseReason::UserQuit;
                                }
                            }
                        }
                        _ => {
                            ask_input.input(key);
                        }
                    },
                    ChatInputMode::FilePath(buf) => match key.code {
                        KeyCode::Esc => input_mode = ChatInputMode::Ask,
                        KeyCode::Enter => {
                            let path = std::path::PathBuf::from(buf.trim());
                            attach_file_to_pending(
                                &ticket_dir,
                                ticket_id,
                                &mut pending_evidence,
                                &chat_tx,
                                &path,
                            )?;
                            input_mode = ChatInputMode::Ask;
                        }
                        KeyCode::Backspace => {
                            buf.pop();
                        }
                        KeyCode::Char(c) => buf.push(c),
                        _ => {}
                    },
                    ChatInputMode::PasteLine(buf) => match key.code {
                        KeyCode::Esc => input_mode = ChatInputMode::Ask,
                        KeyCode::Enter => {
                            if let Some((label, body)) = buf.split_once('=') {
                                let prov = chat::attach_paste(label.trim(), body);
                                let _ = chat_tx.send(chat::ChatEvent::EvidenceAttached {
                                    ts: chrono::Utc::now(),
                                    provenance: prov.clone(),
                                });
                                pending_evidence.push(prov);
                            }
                            input_mode = ChatInputMode::Ask;
                        }
                        KeyCode::Backspace => {
                            buf.pop();
                        }
                        KeyCode::Char(c) => buf.push(c),
                        _ => {}
                    },
                    ChatInputMode::DirPath(buf) => match key.code {
                        KeyCode::Esc => input_mode = ChatInputMode::Ask,
                        KeyCode::Enter => {
                            let raw = buf.trim().to_string();
                            let cmd = parse_chat_command(&format!("/dir {raw}"));
                            if let ChatCommand::Dir {
                                path,
                                recursive,
                                glob,
                            } = cmd
                            {
                                attach_dir_to_pending(
                                    &ticket_dir,
                                    ticket_id,
                                    &mut pending_evidence,
                                    &chat_tx,
                                    &path,
                                    recursive,
                                    glob.as_deref(),
                                )?;
                            }
                            input_mode = ChatInputMode::Ask;
                        }
                        KeyCode::Backspace => {
                            buf.pop();
                        }
                        KeyCode::Char(c) => buf.push(c),
                        _ => {}
                    },
                }
            }
        }
    };

    let _ = chat_tx.send(chat::ChatEvent::SessionClosed {
        ts: chrono::Utc::now(),
        reason: close_reason,
    });
    while let Ok(evt) = chat_rx.try_recv() {
        chat_logger.log(&evt);
    }

    // Tear down the chat terminal before handing control back to the inbox.
    drop(terminal);
    Ok(())
}

fn clear_textarea(input: &mut tui_textarea::TextArea) {
    *input = tui_textarea::TextArea::default();
    input.set_block(ratatui::widgets::Block::default());
}

fn begin_analyst_turn(
    in_flight: &mut Option<crate::chat::ChatProgress>,
    turn_started: &mut Option<std::time::Instant>,
) {
    let started = std::time::Instant::now();
    *turn_started = Some(started);
    *in_flight = Some(crate::chat::initial_turn_progress());
}

fn spawn_analyst_job(
    ticket_dir: std::path::PathBuf,
    ticket_id: String,
    body: String,
    evidence: Vec<crate::models::EvidenceProvenance>,
    provider: std::sync::Arc<dyn crate::providers::LlmProvider>,
    tx: tokio::sync::mpsc::UnboundedSender<crate::chat::ChatEvent>,
) -> ActiveChatJob {
    let (cancel_tx, cancel_rx) =
        if crate::providers::active_codex_transport(provider.as_ref()) == Some("app-server") {
            let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(None);
            (Some(cancel_tx), Some(cancel_rx))
        } else {
            (None, None)
        };
    let handle = tokio::spawn(async move {
        send_analyst_turn_with_progress(
            &ticket_dir,
            &ticket_id,
            &body,
            evidence,
            provider.as_ref(),
            tx,
            cancel_rx,
        )
        .await
        .map_err(|e| e.to_string())
    });
    ActiveChatJob { handle, cancel_tx }
}

fn request_active_job_cancel(
    active_job: Option<&ActiveChatJob>,
    source: crate::chat::CancelSource,
) -> bool {
    let Some(cancel_tx) = active_job.and_then(|job| job.cancel_tx.as_ref()) else {
        return false;
    };
    let cancel = followup_cancel_from_chat_source(source);
    cancel_tx.send(Some(cancel)).is_ok()
}

fn followup_cancel_from_chat_source(
    source: crate::chat::CancelSource,
) -> crate::providers::FollowupCancel {
    match source {
        crate::chat::CancelSource::EscKey => crate::providers::FollowupCancel::EscKey,
        crate::chat::CancelSource::CtrlC => crate::providers::FollowupCancel::CtrlC,
        crate::chat::CancelSource::AppExit => crate::providers::FollowupCancel::CtrlC,
    }
}

fn chat_cancel_source_from_followup(
    cancel: crate::providers::FollowupCancel,
) -> crate::chat::CancelSource {
    match cancel {
        crate::providers::FollowupCancel::EscKey => crate::chat::CancelSource::EscKey,
        crate::providers::FollowupCancel::CtrlC => crate::chat::CancelSource::CtrlC,
    }
}

fn is_interrupted_followup(err: &crate::pipeline::PipelineError) -> bool {
    matches!(
        err,
        crate::pipeline::PipelineError::Followup(crate::pipeline::FollowupError::Provider(
            crate::providers::ProviderError::Interrupted
        ))
    )
}

#[allow(clippy::too_many_arguments)]
fn drain_chat_events(
    chat_rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::chat::ChatEvent>,
    chat_logger: &mut crate::chat::ChatLogger,
    active_job: &mut Option<ActiveChatJob>,
    in_flight: &mut Option<crate::chat::ChatProgress>,
    turn_started: &mut Option<std::time::Instant>,
    status_hint: &mut Option<String>,
    ask_input: &mut tui_textarea::TextArea,
    transcript_follow_bottom: &mut bool,
    ticket_dir: &Path,
    ticket_id: &str,
) {
    while let Ok(evt) = chat_rx.try_recv() {
        chat_logger.log(&evt);
        *in_flight = crate::chat::update_progress(in_flight.take(), &evt);
        apply_terminal_chat_event(
            &evt,
            active_job,
            in_flight,
            turn_started,
            status_hint,
            ask_input,
            transcript_follow_bottom,
            ticket_dir,
            ticket_id,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_terminal_chat_event(
    evt: &crate::chat::ChatEvent,
    active_job: &mut Option<ActiveChatJob>,
    in_flight: &mut Option<crate::chat::ChatProgress>,
    turn_started: &mut Option<std::time::Instant>,
    status_hint: &mut Option<String>,
    ask_input: &mut tui_textarea::TextArea,
    transcript_follow_bottom: &mut bool,
    ticket_dir: &Path,
    ticket_id: &str,
) {
    match evt {
        crate::chat::ChatEvent::TurnPersisted { .. } => {
            active_job.take();
            *in_flight = None;
            *turn_started = None;
            *status_hint = None;
            clear_textarea(ask_input);
            *transcript_follow_bottom = true;
        }
        crate::chat::ChatEvent::ProviderError { message, .. } => {
            active_job.take();
            *in_flight = None;
            *turn_started = None;
            *status_hint = Some(message.clone());
            let _ = append_chat_system_turn(
                ticket_dir,
                ticket_id,
                &format!("follow-up failed: {message}"),
            );
        }
        crate::chat::ChatEvent::Cancelled { .. } => {
            active_job.take();
            *in_flight = None;
            *turn_started = None;
        }
        _ => {}
    }
}

fn next_turn_number(ticket_dir: &Path) -> anyhow::Result<u32> {
    let outcome =
        crate::chat::parse_conversation_jsonl(&crate::chat::conversation_jsonl_path(ticket_dir))
            .map_err(|e| anyhow::anyhow!("parse_conversation_jsonl: {e}"))?;
    Ok(outcome.turns.iter().map(|t| t.turn).max().unwrap_or(0) + 1)
}

fn command_label(cmd: &crate::tui::chat::ChatCommand) -> &'static str {
    match cmd {
        crate::tui::chat::ChatCommand::Body(_) => "send",
        crate::tui::chat::ChatCommand::File(_) => "/file",
        crate::tui::chat::ChatCommand::Dir { .. } => "/dir",
        crate::tui::chat::ChatCommand::Paste { .. } => "/paste",
        crate::tui::chat::ChatCommand::Revise => "/revise",
        crate::tui::chat::ChatCommand::Retry => "/retry",
        crate::tui::chat::ChatCommand::Quit => "/quit",
    }
}

fn latest_analyst_retry_payload(
    turns: &[crate::models::Turn],
) -> Option<(String, Vec<crate::models::EvidenceProvenance>)> {
    turns
        .iter()
        .rev()
        .find(|t| matches!(t.turn_kind, crate::models::TurnKind::Analyst))
        .map(|t| (t.body.clone(), t.evidence.clone()))
}

fn append_chat_system_turn(ticket_dir: &Path, ticket_id: &str, body: &str) -> anyhow::Result<()> {
    let _guard = crate::chat::acquire_session_lock(
        &crate::chat::session_dir(ticket_dir),
        std::time::Duration::from_secs(5),
    )
    .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
    let next = next_turn_number(ticket_dir)?;
    let turn = crate::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: ticket_id.to_string(),
        turn: next,
        turn_kind: crate::models::TurnKind::System,
        ts: chrono::Utc::now(),
        author: None,
        body: body.to_string(),
        evidence: vec![],
        provider: None,
        model: None,
        tokens_in: None,
        tokens_out: None,
        elapsed_s: None,
        session_id: None,
        resumed: None,
        action: None,
        outcome: None,
        drove_revision_from_turns: None,
        diff: None,
    };
    crate::chat::append_turn(&crate::chat::conversation_jsonl_path(ticket_dir), &turn)
        .map_err(|e| anyhow::anyhow!("append system turn: {e}"))?;
    let parsed =
        crate::chat::parse_conversation_jsonl(&crate::chat::conversation_jsonl_path(ticket_dir))?;
    crate::chat::write_conversation_md(
        &crate::chat::conversation_md_path(ticket_dir),
        &parsed.turns,
        ticket_id,
    )?;
    Ok(())
}

fn record_evidence_rejection(
    ticket_dir: &Path,
    ticket_id: &str,
    chat_tx: &tokio::sync::mpsc::UnboundedSender<crate::chat::ChatEvent>,
    reason: String,
    system_body: String,
) {
    let _ = chat_tx.send(crate::chat::ChatEvent::EvidenceRejected {
        ts: chrono::Utc::now(),
        reason,
    });
    let _ = append_chat_system_turn(ticket_dir, ticket_id, &system_body);
}

fn attach_file_to_pending(
    ticket_dir: &Path,
    ticket_id: &str,
    pending_evidence: &mut Vec<crate::models::EvidenceProvenance>,
    chat_tx: &tokio::sync::mpsc::UnboundedSender<crate::chat::ChatEvent>,
    path: &Path,
) -> anyhow::Result<()> {
    let turn_no = next_turn_number(ticket_dir)?;
    match crate::chat::attach_file(ticket_dir, turn_no, path) {
        Ok(prov) => {
            let _ = chat_tx.send(crate::chat::ChatEvent::EvidenceAttached {
                ts: chrono::Utc::now(),
                provenance: prov.clone(),
            });
            pending_evidence.push(prov);
        }
        Err(e) => {
            let reason = format!("attach_file: {e}");
            record_evidence_rejection(
                ticket_dir,
                ticket_id,
                chat_tx,
                reason,
                format!("attach file failed: {e}"),
            );
        }
    }
    Ok(())
}

fn attach_dir_to_pending(
    ticket_dir: &Path,
    ticket_id: &str,
    pending_evidence: &mut Vec<crate::models::EvidenceProvenance>,
    chat_tx: &tokio::sync::mpsc::UnboundedSender<crate::chat::ChatEvent>,
    path: &Path,
    recursive: bool,
    glob: Option<&str>,
) -> anyhow::Result<()> {
    let turn_no = next_turn_number(ticket_dir)?;
    let result = match crate::chat::collect_dir_attachments(
        ticket_dir,
        turn_no,
        path,
        recursive,
        glob,
        25,
        4 * 1024 * 1024,
    ) {
        Ok(result) => result,
        Err(e) => {
            let reason = format!("attach_dir: {e}");
            record_evidence_rejection(
                ticket_dir,
                ticket_id,
                chat_tx,
                reason,
                format!("attach dir failed: {e}"),
            );
            return Ok(());
        }
    };
    let n_attached = result.attached.len();
    let n_skipped = result.skipped.len();
    for provenance in &result.attached {
        let _ = chat_tx.send(crate::chat::ChatEvent::EvidenceAttached {
            ts: chrono::Utc::now(),
            provenance: provenance.clone(),
        });
        pending_evidence.push(provenance.clone());
    }
    for skipped in &result.skipped {
        let _ = chat_tx.send(crate::chat::ChatEvent::EvidenceRejected {
            ts: chrono::Utc::now(),
            reason: format!("{skipped:?}"),
        });
    }
    let mut body = format!(
        "attached {n_attached} file(s) from {}; skipped {n_skipped}.",
        path.display()
    );
    if !result.skipped.is_empty() {
        body.push_str("\nskipped files:");
        for skipped in &result.skipped {
            body.push_str(&format!("\n- {skipped:?}"));
        }
    }
    let _ = append_chat_system_turn(ticket_dir, ticket_id, &body);
    Ok(())
}

/// Convert pending evidence into a (augmented_prompt, attachments) pair.
/// Pastes are inlined into the prompt (they have no file path). Files
/// become Attachment entries; their content flows through the provider's
/// native attachments channel.
fn build_followup_message(
    body: &str,
    evidence: &[crate::models::EvidenceProvenance],
) -> (String, Vec<crate::models::Attachment>) {
    let mut prompt = String::from(body);
    let mut attachments = Vec::new();
    for ev in evidence {
        match ev {
            crate::models::EvidenceProvenance::Paste { label, body, .. } => {
                prompt.push_str(&format!("\n\n## paste: {label}\n{body}"));
            }
            crate::models::EvidenceProvenance::File {
                copied_path,
                basename,
                detected_type,
                ..
            } => {
                let extracted_text =
                    investigation::read_text_if_supported(copied_path, *detected_type)
                        .map(|text| crate::redact::redact(&text).0);
                attachments.push(crate::models::Attachment {
                    copied_path: copied_path.clone(),
                    basename: basename.clone(),
                    detected_type: *detected_type,
                    extracted_text,
                });
            }
        }
    }
    (prompt, attachments)
}

async fn send_analyst_turn_with_progress(
    ticket_dir: &Path,
    ticket_id: &str,
    body: &str,
    evidence: Vec<crate::models::EvidenceProvenance>,
    provider: &dyn crate::providers::LlmProvider,
    tx: tokio::sync::mpsc::UnboundedSender<crate::chat::ChatEvent>,
    cancel_rx: Option<tokio::sync::watch::Receiver<Option<crate::providers::FollowupCancel>>>,
) -> anyhow::Result<()> {
    use crate::chat;
    use std::sync::Arc;

    let model = std::env::var("CODEX_MODEL")
        .unwrap_or_else(|_| crate::providers::codex::DEFAULT_CODEX_MODEL.to_string());
    let bridge_app_server =
        crate::providers::active_codex_transport(provider) == Some("app-server");
    let cancel_source_rx = cancel_rx.as_ref().cloned();
    let turn_instant = std::time::Instant::now();
    let _ = tx.send(chat::ChatEvent::Phase {
        ts: chrono::Utc::now(),
        stage: chat::ChatStage::Ingesting,
        elapsed_s: 0.0,
    });

    // Build the augmented prompt and attachments BEFORE moving `evidence`
    // into the turn record below. Pastes are inlined; files become
    // Attachment entries that flow through the provider's native channel.
    let (augmented_prompt, attachments) = build_followup_message(body, &evidence);
    {
        // Acquire the lock BEFORE computing `next` so a concurrent writer
        // can't sneak an append between our read and our own append, which
        // would collide turn numbers.
        let _guard = chat::acquire_session_lock(
            &chat::session_dir(ticket_dir),
            std::time::Duration::from_secs(5),
        )
        .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let next = next_turn_number(ticket_dir)?;
        let analyst_turn = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: ticket_id.to_string(),
            turn: next,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: chrono::Utc::now(),
            author: std::env::var("TRIAGE_OWNER")
                .or_else(|_| std::env::var("USER"))
                .or_else(|_| std::env::var("USERNAME"))
                .ok(),
            body: body.to_string(),
            evidence,
            provider: None,
            model: None,
            tokens_in: None,
            tokens_out: None,
            elapsed_s: None,
            session_id: None,
            resumed: None,
            action: None,
            outcome: None,
            drove_revision_from_turns: None,
            diff: None,
        };
        chat::append_turn(&chat::conversation_jsonl_path(ticket_dir), &analyst_turn)
            .map_err(|e| anyhow::anyhow!("append_turn: {e}"))?;
        let parsed = chat::parse_conversation_jsonl(&chat::conversation_jsonl_path(ticket_dir))
            .map_err(|e| anyhow::anyhow!("parse: {e}"))?;
        chat::write_conversation_md(
            &chat::conversation_md_path(ticket_dir),
            &parsed.turns,
            ticket_id,
        )
        .map_err(|e| anyhow::anyhow!("write_md: {e}"))?;
        let _ = tx.send(chat::ChatEvent::AnalystAppended {
            ts: chrono::Utc::now(),
            turn: next,
        });
    }
    // The caller-supplied `system_prompt` is intentionally empty here:
    // `pipeline::followup_turn` now owns ticket-context assembly (#22). It
    // rebuilds a bounded, PII-redacted preamble from STATE.md /
    // FORK_PACKET.md / the base-evidence catalog (and, on a Codex
    // session-loss, a bounded replay of prior turns — #23) and prepends it
    // to whatever we pass here. Building it once inside `followup_turn`
    // keeps a single assembly point and avoids a double preamble.
    let reporter = chat::MpscPhaseReporter::new(tx.clone());
    let _ = tx.send(chat::ChatEvent::ProviderRequest {
        ts: chrono::Utc::now(),
        provider: provider.name().to_string(),
        model: model.clone(),
        prompt_bytes: augmented_prompt.len(),
        attachments: attachments.len(),
        session_id: None,
    });
    if bridge_app_server {
        let tx_progress = tx.clone();
        crate::providers::codex_app_server::set_progress_reporter(Some(Arc::new(
            move |progress| chat::bridge_provider_progress(&tx_progress, turn_instant, progress),
        )))
        .await;
    }
    let started = turn_instant;
    let result = pipeline::followup_turn_with_cancel(
        ticket_dir,
        ticket_id,
        &augmented_prompt,
        "",
        &model,
        &attachments,
        provider,
        Some(&reporter),
        cancel_rx,
    )
    .await;
    if bridge_app_server {
        crate::providers::codex_app_server::set_progress_reporter(None).await;
    }

    match result {
        Ok(result) => {
            let _ = tx.send(chat::ChatEvent::ProviderResponse {
                ts: chrono::Utc::now(),
                elapsed_s: started.elapsed().as_secs_f64(),
                tokens_in: result.tokens_in,
                tokens_out: result.tokens_out,
                resumed: result.resumed,
                session_id: result.session_id,
            });
            let outcome =
                chat::parse_conversation_jsonl(&chat::conversation_jsonl_path(ticket_dir))?;
            let codex_turn = outcome
                .turns
                .iter()
                .rev()
                .find(|turn| matches!(turn.turn_kind, crate::models::TurnKind::Codex))
                .map(|turn| turn.turn)
                .unwrap_or(0);
            let _ = tx.send(chat::ChatEvent::TurnPersisted {
                ts: chrono::Utc::now(),
                codex_turn,
            });
            Ok(())
        }
        Err(e) => {
            if is_interrupted_followup(&e) {
                let by = cancel_source_rx
                    .as_ref()
                    .and_then(|rx| *rx.borrow())
                    .map(chat_cancel_source_from_followup)
                    .unwrap_or(chat::CancelSource::EscKey);
                let _ = tx.send(chat::ChatEvent::Cancelled {
                    ts: chrono::Utc::now(),
                    by,
                });
                return Ok(());
            }
            let msg = e.to_string();
            let (redacted_msg, _) = crate::redact::redact(&msg);
            let _ = tx.send(chat::ChatEvent::ProviderError {
                ts: chrono::Utc::now(),
                kind: "followup_turn".into(),
                message: redacted_msg.clone(),
            });
            Err(anyhow::anyhow!("{redacted_msg}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_logger_open_failure_is_returned() {
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        std::fs::write(&ticket_dir, "not a directory").unwrap();

        let err = match open_required_chat_logger(&ticket_dir) {
            Ok(_) => panic!("expected chat logger open to fail"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("chat event logger"));
    }

    #[test]
    fn ctrl_c_is_global_chat_close_key() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

        assert_eq!(
            global_chat_close_reason(key),
            Some(crate::chat::SessionCloseReason::CtrlC)
        );
    }

    #[tokio::test]
    async fn terminal_chat_event_clears_active_job() {
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        std::fs::create_dir_all(crate::chat::session_dir(&ticket_dir)).unwrap();

        let mut active_job = Some(ActiveChatJob {
            handle: tokio::spawn(async { Ok::<(), String>(()) }),
            cancel_tx: None,
        });
        let mut in_flight = Some(crate::chat::initial_turn_progress());
        let mut turn_started = Some(std::time::Instant::now());
        let mut status_hint = Some("working".to_string());
        let mut ask_input = tui_textarea::TextArea::default();
        ask_input.insert_str("what changed?");
        let mut transcript_follow_bottom = false;

        apply_terminal_chat_event(
            &crate::chat::ChatEvent::TurnPersisted {
                ts: chrono::Utc::now(),
                codex_turn: 2,
            },
            &mut active_job,
            &mut in_flight,
            &mut turn_started,
            &mut status_hint,
            &mut ask_input,
            &mut transcript_follow_bottom,
            &ticket_dir,
            "44776",
        );

        assert!(active_job.is_none());
        assert!(in_flight.is_none());
        assert!(turn_started.is_none());
        assert!(status_hint.is_none());
        assert!(ask_input.lines().iter().all(|line| line.is_empty()));
        assert!(transcript_follow_bottom);
    }

    #[test]
    fn build_followup_message_empty_evidence_returns_unchanged() {
        let (prompt, atts) = build_followup_message("ANALYST_QUESTION", &[]);
        assert_eq!(prompt, "ANALYST_QUESTION");
        assert!(atts.is_empty());
    }

    #[test]
    fn build_followup_message_inlines_pastes_and_routes_files_to_attachments() {
        use crate::models::{EvidenceProvenance, ExtractionStatus, FileType};
        use std::io::Write as _;

        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"FILE_CONTENT_SENTINEL\n").unwrap();
        let file_path = tmp.path().to_path_buf();
        let evidence = vec![
            EvidenceProvenance::File {
                source_path: file_path.clone(),
                copied_path: file_path.clone(),
                basename: "diag.log".into(),
                sha256: "0".repeat(64),
                bytes: 22,
                detected_type: FileType::Log,
                extraction: ExtractionStatus::Full,
                truncated: false,
                truncation_note: None,
                sent_to_provider: true,
            },
            EvidenceProvenance::Paste {
                label: "operator-note".into(),
                body: "PASTE_BODY_SENTINEL".into(),
                bytes: 19,
                sent_to_provider: true,
            },
        ];
        let (prompt, atts) = build_followup_message("ANALYST_QUESTION", &evidence);
        // Paste is inlined into the prompt; file is NOT.
        assert!(prompt.contains("ANALYST_QUESTION"));
        assert!(prompt.contains("PASTE_BODY_SENTINEL"));
        assert!(prompt.contains("operator-note"));
        assert!(
            !prompt.contains("FILE_CONTENT_SENTINEL"),
            "file content should not be in prompt; should be in attachments"
        );
        // File becomes an attachment with extracted content.
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].basename, "diag.log");
        assert_eq!(
            atts[0].extracted_text.as_deref().unwrap_or(""),
            "FILE_CONTENT_SENTINEL\n"
        );
    }

    #[test]
    fn build_followup_message_redacts_attachment_text() {
        use crate::models::{EvidenceProvenance, ExtractionStatus, FileType};
        use std::io::Write as _;

        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"call (555) 123-4567\n").unwrap();
        let file_path = tmp.path().to_path_buf();
        let evidence = vec![EvidenceProvenance::File {
            source_path: file_path.clone(),
            copied_path: file_path,
            basename: "diag.log".into(),
            sha256: "0".repeat(64),
            bytes: 20,
            detected_type: FileType::Log,
            extraction: ExtractionStatus::Full,
            truncated: false,
            truncation_note: None,
            sent_to_provider: true,
        }];

        let (_prompt, atts) = build_followup_message("question", &evidence);
        let text = atts[0].extracted_text.as_deref().unwrap_or("");
        assert!(!text.contains("123-4567"), "PII leaked: {text}");
        assert!(text.contains("<PHONE>"), "redaction marker missing: {text}");
    }

    #[test]
    fn latest_analyst_retry_payload_preserves_evidence() {
        use crate::models::{EvidenceProvenance, Turn, TurnKind};

        fn turn(
            turn: u32,
            turn_kind: TurnKind,
            body: &str,
            evidence: Vec<EvidenceProvenance>,
        ) -> Turn {
            Turn {
                schema: "triage-cli/conversation".into(),
                schema_version: 1,
                ticket_id: "44776".into(),
                turn,
                turn_kind,
                ts: chrono::Utc::now(),
                author: None,
                body: body.into(),
                evidence,
                provider: None,
                model: None,
                tokens_in: None,
                tokens_out: None,
                elapsed_s: None,
                session_id: None,
                resumed: None,
                action: None,
                outcome: None,
                drove_revision_from_turns: None,
                diff: None,
            }
        }

        let evidence = vec![EvidenceProvenance::Paste {
            label: "operator-note".into(),
            body: "same evidence".into(),
            bytes: 13,
            sent_to_provider: true,
        }];
        let turns = vec![
            turn(1, TurnKind::System, "system", vec![]),
            turn(2, TurnKind::Analyst, "retry this", evidence),
            turn(3, TurnKind::Codex, "answer", vec![]),
        ];

        let (body, evidence) = latest_analyst_retry_payload(&turns).unwrap();
        assert_eq!(body, "retry this");
        assert_eq!(evidence.len(), 1);
        match &evidence[0] {
            EvidenceProvenance::Paste { label, body, .. } => {
                assert_eq!(label, "operator-note");
                assert_eq!(body, "same evidence");
            }
            other => panic!("expected paste evidence, got {other:?}"),
        }
    }

    #[test]
    fn attach_file_to_pending_rejects_missing_path_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut pending = Vec::new();
        let missing = dir.path().join("missing.log");

        attach_file_to_pending(&ticket_dir, "44776", &mut pending, &tx, &missing).unwrap();

        assert!(pending.is_empty());
        let evt = rx.try_recv().expect("expected rejection event");
        assert!(matches!(
            evt,
            crate::chat::ChatEvent::EvidenceRejected { ref reason, .. }
                if reason.contains("attach_file")
        ));
        let parsed = crate::chat::parse_conversation_jsonl(&crate::chat::conversation_jsonl_path(
            &ticket_dir,
        ))
        .unwrap();
        assert_eq!(parsed.turns.len(), 1);
        assert!(parsed.turns[0].body.contains("attach file failed"));
    }

    #[test]
    fn attach_dir_to_pending_rejects_missing_dir_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut pending = Vec::new();
        let missing = dir.path().join("missing-dir");

        attach_dir_to_pending(
            &ticket_dir,
            "44776",
            &mut pending,
            &tx,
            &missing,
            true,
            None,
        )
        .unwrap();

        assert!(pending.is_empty());
        let evt = rx.try_recv().expect("expected rejection event");
        assert!(matches!(
            evt,
            crate::chat::ChatEvent::EvidenceRejected { ref reason, .. }
                if reason.contains("attach_dir")
        ));
        let parsed = crate::chat::parse_conversation_jsonl(&crate::chat::conversation_jsonl_path(
            &ticket_dir,
        ))
        .unwrap();
        assert_eq!(parsed.turns.len(), 1);
        assert!(parsed.turns[0].body.contains("attach dir failed"));
    }

    #[test]
    fn append_chat_system_turn_waits_for_session_lock() {
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        std::fs::create_dir_all(crate::chat::session_dir(&ticket_dir)).unwrap();
        let guard = crate::chat::acquire_session_lock(
            &crate::chat::session_dir(&ticket_dir),
            std::time::Duration::from_secs(1),
        )
        .unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let thread_ticket_dir = ticket_dir.clone();
        let handle = std::thread::spawn(move || {
            let result = append_chat_system_turn(&thread_ticket_dir, "44776", "system note");
            tx.send(result).unwrap();
        });

        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(150))
                .is_err(),
            "system turn appended while another session held the ticket lock"
        );
        drop(guard);

        rx.recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        handle.join().unwrap();
        let parsed = crate::chat::parse_conversation_jsonl(&crate::chat::conversation_jsonl_path(
            &ticket_dir,
        ))
        .unwrap();
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].turn, 1);
    }

    #[tokio::test]
    async fn event_loop_logs_full_turn_sequence_via_mpsc() {
        use crate::chat::{chat_events_log_path, ChatEvent, ChatLogger, ChatStage};

        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        std::fs::create_dir_all(crate::chat::session_dir(&ticket_dir)).unwrap();

        struct StubProvider;
        impl crate::providers::LlmProvider for StubProvider {
            fn name(&self) -> &'static str {
                "stub"
            }

            fn complete<'a>(
                &'a self,
                _prompt: &'a str,
                _system_prompt: &'a str,
                _model: &'a str,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                crate::providers::CompletionResult,
                                crate::providers::ProviderError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async {
                    Ok(crate::providers::CompletionResult {
                        text: "stub answer".into(),
                        tokens_in: Some(10),
                        tokens_out: Some(20),
                    })
                })
            }
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
        let mut logger = ChatLogger::open(&ticket_dir).unwrap();
        send_analyst_turn_with_progress(
            &ticket_dir,
            "44776",
            "what changed?",
            Vec::new(),
            &StubProvider,
            tx,
            None,
        )
        .await
        .unwrap();

        let mut kinds = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            kinds.push(match &evt {
                ChatEvent::Phase {
                    stage: ChatStage::Ingesting,
                    ..
                } => "phase:ingesting",
                ChatEvent::Phase {
                    stage: ChatStage::ContextAssembled,
                    ..
                } => "phase:context",
                ChatEvent::Phase {
                    stage: ChatStage::ProviderAwait,
                    ..
                } => "phase:await",
                ChatEvent::Phase {
                    stage: ChatStage::ResponseParsed,
                    ..
                } => "phase:parsed",
                ChatEvent::Phase {
                    stage: ChatStage::Saved,
                    ..
                } => "phase:saved",
                ChatEvent::AnalystAppended { .. } => "analyst",
                ChatEvent::ProviderRequest { .. } => "req",
                ChatEvent::ProviderResponse { .. } => "resp",
                ChatEvent::TurnPersisted { .. } => "persisted",
                other => panic!("unexpected event: {other:?}"),
            });
            logger.log(&evt);
        }
        drop(logger);

        assert_eq!(
            kinds,
            vec![
                "phase:ingesting",
                "analyst",
                "req",
                "phase:context",
                "phase:await",
                "phase:parsed",
                "phase:saved",
                "resp",
                "persisted",
            ]
        );
        let log_body = std::fs::read_to_string(chat_events_log_path(&ticket_dir)).unwrap();
        assert_eq!(log_body.lines().count(), 9);
    }

    #[tokio::test]
    async fn interrupted_followup_logs_cancel_without_provider_turn() {
        use crate::chat::ChatEvent;

        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        std::fs::create_dir_all(crate::chat::session_dir(&ticket_dir)).unwrap();

        struct InterruptedProvider;
        impl crate::providers::LlmProvider for InterruptedProvider {
            fn name(&self) -> &'static str {
                "codex"
            }

            fn complete<'a>(
                &'a self,
                _prompt: &'a str,
                _system_prompt: &'a str,
                _model: &'a str,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                crate::providers::CompletionResult,
                                crate::providers::ProviderError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async { Err(crate::providers::ProviderError::Interrupted) })
            }

            fn followup_with_cancel<'a>(
                &'a self,
                _session_id: Option<&'a str>,
                _prompt: &'a str,
                _system_prompt: &'a str,
                _model: &'a str,
                _attachments: &'a [crate::models::Attachment],
                _cancel_rx: Option<
                    tokio::sync::watch::Receiver<Option<crate::providers::FollowupCancel>>,
                >,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                crate::providers::FollowupResult,
                                crate::providers::ProviderError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async { Err(crate::providers::ProviderError::Interrupted) })
            }
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
        let (_cancel_tx, cancel_rx) =
            tokio::sync::watch::channel(Some(crate::providers::FollowupCancel::CtrlC));
        send_analyst_turn_with_progress(
            &ticket_dir,
            "44776",
            "cancel me",
            Vec::new(),
            &InterruptedProvider,
            tx,
            Some(cancel_rx),
        )
        .await
        .unwrap();

        let mut cancelled_by = None;
        let mut saw_provider_error = false;
        while let Ok(evt) = rx.try_recv() {
            match evt {
                ChatEvent::Cancelled { by, .. } => cancelled_by = Some(by),
                ChatEvent::ProviderError { .. } => saw_provider_error = true,
                _ => {}
            }
        }
        assert_eq!(
            cancelled_by,
            Some(crate::chat::CancelSource::CtrlC),
            "interrupted followup must preserve the requested cancel source"
        );
        assert!(
            !saw_provider_error,
            "interrupted followup must not log ProviderError"
        );
        let parsed = crate::chat::parse_conversation_jsonl(&crate::chat::conversation_jsonl_path(
            &ticket_dir,
        ))
        .unwrap();
        assert_eq!(parsed.turns.len(), 1);
        assert!(matches!(
            parsed.turns[0].turn_kind,
            crate::models::TurnKind::Analyst
        ));
        crate::chat::acquire_session_lock(
            &crate::chat::session_dir(&ticket_dir),
            std::time::Duration::from_millis(50),
        )
        .expect("interrupted followup must release session lock");
    }

    #[tokio::test]
    async fn collect_dir_then_log_attached_and_rejected() {
        use crate::chat::{chat_events_log_path, ChatEvent, ChatLogger, DirSkipped};

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("logs");
        std::fs::create_dir_all(&src).unwrap();
        for i in 0..30u32 {
            std::fs::write(src.join(format!("a{i:03}.log")), "x").unwrap();
        }
        let ticket_dir = dir.path().join("44776");
        std::fs::create_dir_all(crate::chat::session_dir(&ticket_dir)).unwrap();

        let result =
            crate::chat::collect_dir_attachments(&ticket_dir, 1, &src, false, None, 25, 4 << 20)
                .unwrap();
        assert_eq!(result.attached.len(), 25);
        assert_eq!(result.skipped.len(), 1);

        let mut logger = ChatLogger::open(&ticket_dir).unwrap();
        for provenance in &result.attached {
            logger.log(&ChatEvent::EvidenceAttached {
                ts: chrono::Utc::now(),
                provenance: provenance.clone(),
            });
        }
        for skipped in &result.skipped {
            let reason = match skipped {
                DirSkipped::ScanCapReached { path, limit } => {
                    format!("scan_cap: {} after {limit} files", path.display())
                }
                other => format!("{other:?}"),
            };
            logger.log(&ChatEvent::EvidenceRejected {
                ts: chrono::Utc::now(),
                reason,
            });
        }
        drop(logger);

        let body = std::fs::read_to_string(chat_events_log_path(&ticket_dir)).unwrap();
        assert_eq!(
            body.lines()
                .filter(|line| line.contains("evidence_attached"))
                .count(),
            25
        );
        assert_eq!(
            body.lines()
                .filter(|line| line.contains("evidence_rejected"))
                .count(),
            1
        );
    }
}
