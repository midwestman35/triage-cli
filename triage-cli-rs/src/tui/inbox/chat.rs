use std::path::Path;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tui_textarea::TextArea;

use crate::datadog::{DatadogClient, DatadogSource};
use crate::investigation;
use crate::pipeline;
use crate::providers::get_provider;
use crate::ticket_folder;
use crate::tui::chat::{parse_chat_command, ChatCommand};

pub(crate) async fn run_chat_session(ticket_id: &str) -> anyhow::Result<()> {
    let ticket_dir = ticket_folder::tickets_root().join(ticket_id);
    std::fs::create_dir_all(&ticket_dir)?;

    enable_raw_mode()?;
    let stderr = std::io::stderr();
    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;

    let mut input = TextArea::default();
    input.set_block(ratatui::widgets::Block::default());

    let mut pending_evidence: Vec<crate::models::EvidenceProvenance> = Vec::new();
    let mut in_flight: Option<crate::tui::chat::InFlightState> = None;

    let provider = get_provider().map_err(|e| anyhow::anyhow!("provider unavailable: {e}"))?;

    loop {
        let outcome = crate::chat::parse_conversation_jsonl(
            &crate::chat::conversation_jsonl_path(&ticket_dir),
        )?;
        let pane = crate::tui::chat::ChatPane {
            turns: &outcome.turns,
            input: &input,
            ticket_id,
            in_flight: in_flight.clone(),
        };
        terminal.draw(|f| {
            let area = f.area();
            f.render_widget(&pane, area);
        })?;

        if let Some(ref mut s) = in_flight {
            s.frame_idx = s.frame_idx.wrapping_add(1);
        }

        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = event::read()? {
                match (key.code, key.modifiers) {
                    (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,
                    (KeyCode::Char('s'), KeyModifiers::CONTROL) => {
                        let body: String = input.lines().join("\n");
                        if body.trim().is_empty() {
                            continue;
                        }
                        match parse_chat_command(&body) {
                            ChatCommand::Body(b) => {
                                send_analyst_turn(
                                    &ticket_dir,
                                    ticket_id,
                                    &b,
                                    std::mem::take(&mut pending_evidence),
                                    provider.as_ref(),
                                    &mut input,
                                )
                                .await?;
                            }
                            ChatCommand::File(path) => {
                                let turn_no = next_turn_number(&ticket_dir)?;
                                let prov = crate::chat::attach_file(&ticket_dir, turn_no, &path)
                                    .map_err(|e| anyhow::anyhow!("attach_file: {e}"))?;
                                pending_evidence.push(prov);
                                clear_textarea(&mut input);
                            }
                            ChatCommand::Paste { label, body } => {
                                pending_evidence.push(crate::chat::attach_paste(&label, &body));
                                clear_textarea(&mut input);
                            }
                            ChatCommand::Revise => {
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
                                clear_textarea(&mut input);
                            }
                            ChatCommand::Retry => {
                                let retry_outcome = crate::chat::parse_conversation_jsonl(
                                    &crate::chat::conversation_jsonl_path(&ticket_dir),
                                )?;
                                if let Some(last_analyst) =
                                    retry_outcome.turns.iter().rev().find(|t| {
                                        matches!(t.turn_kind, crate::models::TurnKind::Analyst)
                                    })
                                {
                                    let body_clone = last_analyst.body.clone();
                                    send_analyst_turn(
                                        &ticket_dir,
                                        ticket_id,
                                        &body_clone,
                                        Vec::new(),
                                        provider.as_ref(),
                                        &mut input,
                                    )
                                    .await?;
                                }
                            }
                            ChatCommand::Quit => break,
                        }
                    }
                    _ => {
                        input.input(key);
                    }
                }
            }
        }
    }

    let _ = disable_raw_mode();
    drop(terminal);
    Ok(())
}

fn clear_textarea(input: &mut TextArea) {
    *input = TextArea::default();
    input.set_block(ratatui::widgets::Block::default());
}

fn next_turn_number(ticket_dir: &Path) -> anyhow::Result<u32> {
    let outcome =
        crate::chat::parse_conversation_jsonl(&crate::chat::conversation_jsonl_path(ticket_dir))
            .map_err(|e| anyhow::anyhow!("parse_conversation_jsonl: {e}"))?;
    Ok(outcome.turns.iter().map(|t| t.turn).max().unwrap_or(0) + 1)
}

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
                    investigation::read_text_if_supported(copied_path, *detected_type);
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

async fn send_analyst_turn(
    ticket_dir: &Path,
    ticket_id: &str,
    body: &str,
    evidence: Vec<crate::models::EvidenceProvenance>,
    provider: &dyn crate::providers::LlmProvider,
    input: &mut TextArea<'_>,
) -> anyhow::Result<()> {
    let (augmented_prompt, attachments) = build_followup_message(body, &evidence);
    {
        let _guard = crate::chat::acquire_session_lock(
            &crate::chat::session_dir(ticket_dir),
            Duration::from_secs(5),
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
        crate::chat::append_turn(
            &crate::chat::conversation_jsonl_path(ticket_dir),
            &analyst_turn,
        )
        .map_err(|e| anyhow::anyhow!("append_turn: {e}"))?;
        let parsed = crate::chat::parse_conversation_jsonl(&crate::chat::conversation_jsonl_path(
            ticket_dir,
        ))
        .map_err(|e| anyhow::anyhow!("parse: {e}"))?;
        crate::chat::write_conversation_md(
            &crate::chat::conversation_md_path(ticket_dir),
            &parsed.turns,
            ticket_id,
        )
        .map_err(|e| anyhow::anyhow!("write_md: {e}"))?;
    }

    let _result = pipeline::followup_turn(
        ticket_dir,
        ticket_id,
        &augmented_prompt,
        "",
        &std::env::var("CODEX_MODEL")
            .unwrap_or_else(|_| crate::providers::codex::DEFAULT_CODEX_MODEL.to_string()),
        &attachments,
        provider,
    )
    .await?;
    clear_textarea(input);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(prompt.contains("ANALYST_QUESTION"));
        assert!(prompt.contains("PASTE_BODY_SENTINEL"));
        assert!(prompt.contains("operator-note"));
        assert!(!prompt.contains("FILE_CONTENT_SENTINEL"));
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].basename, "diag.log");
        assert_eq!(
            atts[0].extracted_text.as_deref().unwrap_or(""),
            "FILE_CONTENT_SENTINEL\n"
        );
    }
}
