use std::time::Duration;

use crate::chat;
use crate::datadog::DatadogSource;
use crate::models::{BaseEvidenceManifest, InvestigationSession, Ticket, Turn};
use crate::ticket_folder;
use crate::zendesk::ZendeskSource;

use super::investigate::investigate_one_structured;
use super::options::InvestigateOptions;
use super::reporter::SilentReporter;
use super::{FollowupError, PipelineError};

/// Build the synthetic `InvestigationSession` that `/revise` feeds into
/// `investigate_one_structured`. Seeds the session with the base-evidence
/// catalog (so the LLM re-emission preserves the original `E-NNN`
/// identifiers and labels) AND, for entries where the v2 manifest carries
/// a `body` snapshot, injects the captured body as a labeled paste so the
/// LLM re-emission sees the same signal that drove the original fork.
/// Then layers post-base evidence from analyst/automated turns recorded
/// since `last_revise_turn`.
///
/// **Schema v1 backward compatibility:** legacy manifests deserialize
/// into v2 with `entry.body == None`; those entries surface only in the
/// catalog summary, matching the pre-ADR-0003 behavior.
///
/// Extracted into a free function so it can be unit-tested directly (the
/// no-llm pipeline path stubs the output and doesn't reveal what the
/// session actually contains).
pub(crate) fn build_revise_session(
    base_ticket: &Ticket,
    base_evidence: &BaseEvidenceManifest,
    turns: &[Turn],
    last_revise_turn: u32,
) -> InvestigationSession {
    let mut session = crate::investigation::create_session(base_ticket.clone());

    // 1. Surface base-evidence catalog as a labeled paste so the LLM
    //    re-emission knows what evidence existed originally.
    if !base_evidence.evidence.is_empty() {
        let mut catalog = String::from("Base-evidence catalog from the original investigation:\n");
        for entry in &base_evidence.evidence {
            catalog.push_str(&format!(
                "- {} [{}] {}\n",
                entry.item.id, entry.item.kind, entry.item.label
            ));
        }
        crate::investigation::add_pasted_evidence(&mut session, "base-evidence-catalog", &catalog);
    }

    // 2. For each entry that carries a body snapshot (v2 manifests), inject
    //    the body as a labeled paste so the LLM re-emission has the same
    //    raw signal the original investigation did. Entries without a body
    //    (legacy v1 manifests, or kinds where extraction wasn't possible)
    //    surface only via the catalog above.
    for entry in &base_evidence.evidence {
        if let Some(body) = &entry.body {
            let label = format!("base-{}-{}", entry.item.kind, entry.item.id);
            crate::investigation::add_pasted_evidence(&mut session, &label, body);
        }
    }

    // 3. Layer post-base evidence from turns since the last revise.
    for t in turns.iter().filter(|t| t.turn > last_revise_turn) {
        for ev in &t.evidence {
            match ev {
                crate::models::EvidenceProvenance::File { copied_path, .. } => {
                    // Best-effort: if the copied file has been removed we skip it.
                    let _ = crate::investigation::add_local_file(&mut session, copied_path);
                }
                crate::models::EvidenceProvenance::Paste { label, body, .. } => {
                    crate::investigation::add_pasted_evidence(&mut session, label, body);
                }
            }
        }
        // Also feed analyst-turn body text as a labeled paste so the
        // structured pipeline sees what the analyst told us.
        if matches!(t.turn_kind, crate::models::TurnKind::Analyst) && !t.body.is_empty() {
            crate::investigation::add_pasted_evidence(
                &mut session,
                &format!("turn-{:03}-body", t.turn),
                &t.body,
            );
        }
    }

    session
}

/// `/revise` re-entry. Validates that there is new evidence since the
/// last revise, loads base snapshots, builds a synthetic
/// `InvestigationSession`, and calls `investigate_one_structured` with
/// `followup_mode=true` to re-emit the five-markdown folder.  Then
/// appends a system revise turn to CONVERSATION.jsonl so the chat pane
/// shows the revision event.
///
/// `opts` is forwarded to `investigate_one_structured`; the caller should
/// set `no_llm: true` in tests / dry-run mode.  `followup_mode` is
/// always forced to `true` regardless of what `opts` contains.
pub async fn revise(
    ticket_dir: &std::path::Path,
    ticket_id: &str,
    zd_client: Option<&dyn ZendeskSource>,
    dd_client: Option<&dyn DatadogSource>,
    opts: &InvestigateOptions,
) -> Result<(), PipelineError> {
    // Acquire the per-ticket lock for the duration of the revise.
    let session_dir = chat::session_dir(ticket_dir);
    let _guard =
        chat::acquire_session_lock(&session_dir, Duration::from_secs(5)).map_err(|e| match e {
            chat::ChatError::LockContention { lock_path } => {
                FollowupError::LockContention(lock_path)
            }
            other => FollowupError::Chat(other),
        })?;

    // Load the conversation and find the last system/revise turn number.
    let conv = chat::conversation_jsonl_path(ticket_dir);
    let outcome = chat::parse_conversation_jsonl(&conv).map_err(FollowupError::Chat)?;
    let last_revise_turn = outcome
        .turns
        .iter()
        .rev()
        .find(|t| {
            matches!(t.turn_kind, crate::models::TurnKind::System)
                && t.action.as_deref() == Some("revise")
        })
        .map(|t| t.turn)
        .unwrap_or(0);

    // Validate: at least one new analyst-or-automated turn since the last
    // revise must carry new evidence (file or labeled paste). A
    // question-only turn does NOT qualify.
    let new_evidence_present = outcome.turns.iter().any(|t| {
        t.turn > last_revise_turn
            && matches!(
                t.turn_kind,
                crate::models::TurnKind::Analyst | crate::models::TurnKind::Automated
            )
            && !t.evidence.is_empty()
    });
    if !new_evidence_present {
        return Err(PipelineError::Followup(FollowupError::BaseSnapshotMissing(
            "no new evidence since last /revise; attach a file or labeled paste before revising"
                .to_string(),
        )));
    }

    // Load base snapshots.
    let base_ticket = chat::read_base_ticket(ticket_dir)
        .map_err(|e| FollowupError::BaseSnapshotMissing(e.to_string()))?;
    let base_evidence = chat::read_base_evidence_manifest(ticket_dir)
        .map_err(|e| FollowupError::BaseSnapshotMissing(e.to_string()))?;

    // --- Structured re-emission (spec § 2.5 V1 ship list) ---
    // Build a synthetic InvestigationSession from the base ticket, the base
    // evidence catalog, and any new evidence (file attachments and labeled
    // pastes) extracted from CONVERSATION.jsonl turns since the last revise.
    // Then run the structured pipeline with followup_mode=true so the
    // five-markdown folder is rewritten while the base snapshots are preserved.
    let mut session = build_revise_session(
        &base_ticket,
        &base_evidence,
        &outcome.turns,
        last_revise_turn,
    );

    let rubric = crate::playbook::Rubric::load()
        .map_err(|e| FollowupError::BaseSnapshotMissing(format!("rubric load failed: {e}")))?;

    // Build the options for the structured re-emission. followup_mode is always
    // true on /revise. `force` is propagated from the caller so /revise honors
    // the STATE.md owner soft-lock (the per-ticket session lock acquired above
    // is orthogonal — it serializes concurrent writers on this ticket, but
    // does NOT authorize one analyst to overwrite another's STATE.md).
    //
    // `tickets_root` is set to the existing ticket_dir's parent so that the
    // structured pipeline writes back to the same folder we're revising,
    // without mutating process-global TRIAGE_TICKETS_ROOT.
    let tickets_root_parent = ticket_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(ticket_folder::tickets_root);
    let revise_opts = InvestigateOptions {
        followup_mode: true,
        force: opts.force,
        tickets_root: Some(tickets_root_parent),
        no_llm: opts.no_llm,
        redact_enabled: opts.redact_enabled,
        verbose: opts.verbose,
        memory_hits_override: opts.memory_hits_override.clone(),
        customer_history_override: opts.customer_history_override.clone(),
        ..InvestigateOptions::defaults()
    };

    let reporter = SilentReporter;
    // `dd_client` is forwarded so callers that have a live DatadogSource
    // (e.g. the inbox /revise handler) can re-fetch logs around the original
    // anchor. When `None`, the pipeline skips DD entirely and relies on the
    // base-evidence catalog plus whatever the analyst attached this turn.
    let _structured = investigate_one_structured(
        base_ticket.clone(),
        &mut session,
        zd_client,
        dd_client,
        &rubric,
        &reporter,
        &revise_opts,
    )
    .await?;

    let next_turn = outcome.turns.iter().map(|t| t.turn).max().unwrap_or(0) + 1;
    let driving_turns: Vec<u32> = outcome
        .turns
        .iter()
        .filter(|t| t.turn > last_revise_turn)
        .map(|t| t.turn)
        .collect();

    let system_turn = crate::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: ticket_id.to_string(),
        turn: next_turn,
        turn_kind: crate::models::TurnKind::System,
        ts: chrono::Utc::now(),
        author: None,
        body: format!(
            "Revise validated using base ticket id {}. {} turn(s) since last revise carry new evidence.",
            base_ticket.id,
            driving_turns.len(),
        ),
        evidence: vec![],
        provider: None,
        model: None,
        tokens_in: None,
        tokens_out: None,
        elapsed_s: None,
        session_id: None,
        resumed: None,
        action: Some("revise".to_string()),
        outcome: Some("validated".to_string()),
        drove_revision_from_turns: Some(driving_turns),
        diff: None,
    };
    chat::append_turn(&conv, &system_turn).map_err(FollowupError::Chat)?;

    let parsed = chat::parse_conversation_jsonl(&conv).map_err(FollowupError::Chat)?;
    chat::write_conversation_md(
        &chat::conversation_md_path(ticket_dir),
        &parsed.turns,
        ticket_id,
    )
    .map_err(FollowupError::Chat)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_revise_session_surfaces_base_evidence_catalog() {
        // /revise must surface the base-evidence catalog (E-NNN ids + labels
        // recorded during the original investigation) so the LLM knows what
        // signal originally drove the fork. The manifest stores only a
        // catalog — bodies live in source files / external systems — so we
        // can't fully restore content, but we can pass the labeled list
        // forward as a synthetic context paste.
        //
        // Post-base evidence (file/paste added in follow-up turns) must also
        // make it into the session.
        let base_ticket = crate::models::Ticket {
            id: 99001,
            subject: "audio dropped".into(),
            description: "initial intake".into(),
            requester_org: None,
            requester_email: None,
            tags: vec![],
            created_at: chrono::Utc::now(),
            updated_at: None,
            comments: vec![],
        };
        let base_evidence = crate::models::BaseEvidenceManifest {
            schema: "triage-cli/base-evidence".into(),
            schema_version: 2,
            ticket_id: "99001".into(),
            captured_at: chrono::Utc::now(),
            evidence: vec![
                crate::models::BaseEvidenceEntry {
                    item: crate::models::EvidenceItem {
                        id: "E-001".into(),
                        kind: "datadog_log_window".into(),
                        label: "JeffCom 2026-05-13T07:00 to 07:30".into(),
                        source_time: None,
                        source_path: "datadog:log_window".into(),
                    },
                    body: None,
                },
                crate::models::BaseEvidenceEntry {
                    item: crate::models::EvidenceItem {
                        id: "E-002".into(),
                        kind: "local_file".into(),
                        label: "apex.log".into(),
                        source_time: None,
                        source_path: "local:apex.log".into(),
                    },
                    body: None,
                },
            ],
        };
        let post_base_turn = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "99001".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: chrono::Utc::now(),
            author: Some("enrique".into()),
            body: "follow-up question".into(),
            evidence: vec![crate::models::EvidenceProvenance::Paste {
                label: "new-paste".into(),
                body: "NEW_EVIDENCE_SENTINEL".into(),
                bytes: 21,
                sent_to_provider: true,
            }],
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

        let session = build_revise_session(&base_ticket, &base_evidence, &[post_base_turn], 0);

        let pasted_texts: Vec<&str> = session
            .evidence
            .pasted_logs
            .iter()
            .map(|p| p.text.as_str())
            .collect();
        // Base-evidence catalog must be surfaced (E-001 and E-002 ids preserved).
        let catalog_text = pasted_texts
            .iter()
            .find(|s| s.contains("E-001") && s.contains("E-002"))
            .copied()
            .unwrap_or_else(|| {
                panic!("base-evidence catalog missing; pasted_logs = {pasted_texts:?}")
            });
        assert!(
            catalog_text.contains("datadog_log_window") && catalog_text.contains("apex.log"),
            "catalog summary does not name original evidence kinds/labels: {catalog_text}"
        );
        // Post-base evidence must also be in the session.
        assert!(
            pasted_texts
                .iter()
                .any(|s| s.contains("NEW_EVIDENCE_SENTINEL")),
            "post-base evidence missing; pasted_logs = {pasted_texts:?}"
        );
    }

    #[test]
    fn build_revise_session_injects_body_snapshots() {
        // /revise must inject each non-None body snapshot from the v2
        // manifest as a labeled paste in the synthetic session, so the LLM
        // re-emission sees the raw signal that drove the original fork
        // (not just the E-NNN catalog).
        let base_ticket = crate::models::Ticket {
            id: 99002,
            subject: "audio stutter".into(),
            description: "".into(),
            requester_org: None,
            requester_email: None,
            tags: vec![],
            created_at: chrono::Utc::now(),
            updated_at: None,
            comments: vec![],
        };
        let base_evidence = crate::models::BaseEvidenceManifest {
            schema: "triage-cli/base-evidence".into(),
            schema_version: 2,
            ticket_id: "99002".into(),
            captured_at: chrono::Utc::now(),
            evidence: vec![
                crate::models::BaseEvidenceEntry {
                    item: crate::models::EvidenceItem {
                        id: "E-001".into(),
                        kind: "datadog_log_window".into(),
                        label: "site window".into(),
                        source_time: None,
                        source_path: "datadog:log_window".into(),
                    },
                    body: Some("DD_LOG_BODY_SENTINEL".into()),
                },
                crate::models::BaseEvidenceEntry {
                    item: crate::models::EvidenceItem {
                        id: "E-002".into(),
                        kind: "local_file".into(),
                        label: "apex.log".into(),
                        source_time: None,
                        source_path: "local:apex.log".into(),
                    },
                    body: Some("LOCAL_FILE_BODY_SENTINEL".into()),
                },
                crate::models::BaseEvidenceEntry {
                    item: crate::models::EvidenceItem {
                        id: "E-003".into(),
                        kind: "pasted_note".into(),
                        label: "customer-note".into(),
                        source_time: None,
                        source_path: "pasted:customer-note".into(),
                    },
                    // Legacy entry without a body should be silently
                    // dropped from the body-injection pass — only the
                    // catalog summary mentions it.
                    body: None,
                },
            ],
        };

        let session = build_revise_session(&base_ticket, &base_evidence, &[], 0);
        let pasted_texts: Vec<&str> = session
            .evidence
            .pasted_logs
            .iter()
            .map(|p| p.text.as_str())
            .collect();
        // 1 catalog paste + 2 body pastes (E-001, E-002); E-003 body is None
        // and must NOT produce an extra paste.
        assert_eq!(
            session.evidence.pasted_logs.len(),
            3,
            "expected 3 pasted_logs (1 catalog + 2 bodies); got {}; logs = {pasted_texts:?}",
            session.evidence.pasted_logs.len()
        );
        assert!(
            pasted_texts
                .iter()
                .any(|s| s.contains("DD_LOG_BODY_SENTINEL")),
            "datadog body was not injected; pasted_logs = {pasted_texts:?}"
        );
        assert!(
            pasted_texts
                .iter()
                .any(|s| s.contains("LOCAL_FILE_BODY_SENTINEL")),
            "local-file body was not injected; pasted_logs = {pasted_texts:?}"
        );
        // The catalog summary must still be present (with all three
        // E-NNN ids) even though E-003 has no body.
        let catalog = pasted_texts
            .iter()
            .find(|s| s.contains("E-001") && s.contains("E-002") && s.contains("E-003"))
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "base-evidence catalog missing or incomplete; pasted_logs = {pasted_texts:?}"
                )
            });
        assert!(catalog.contains("datadog_log_window"));
        assert!(catalog.contains("pasted_note"));
    }

    #[test]
    fn current_owner_falls_back_to_username_when_user_unset() {
        use super::super::owner::current_owner;
        // Save and clear both vars so we test in a known state.
        let prev_user = std::env::var("USER").ok();
        let prev_username = std::env::var("USERNAME").ok();
        let prev_triage_owner = std::env::var("TRIAGE_OWNER").ok();

        std::env::remove_var("USER");
        std::env::remove_var("TRIAGE_OWNER");
        std::env::set_var("USERNAME", "alice");

        assert_eq!(current_owner(), "alice");

        // Restore.
        match prev_user {
            Some(v) => std::env::set_var("USER", v),
            None => std::env::remove_var("USER"),
        }
        match prev_username {
            Some(v) => std::env::set_var("USERNAME", v),
            None => std::env::remove_var("USERNAME"),
        }
        match prev_triage_owner {
            Some(v) => std::env::set_var("TRIAGE_OWNER", v),
            None => std::env::remove_var("TRIAGE_OWNER"),
        }
    }
}
