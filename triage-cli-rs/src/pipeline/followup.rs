use std::time::Duration;

use crate::chat;

use super::{FollowupError, PipelineError};

/// Action value written to the System turn that signals a codex session was
/// lost (provider failed to resume the prior session and started fresh).
/// Extracted to a const so implementation and tests stay in sync.
pub const SESSION_LOST_ACTION: &str = "session_lost";

/// Whether manifest transport matches the active Codex transport.
/// Missing `codex_transport` on legacy manifests is treated as aligned.
fn codex_transport_aligns(
    manifest: &crate::models::SessionManifest,
    active_transport: &str,
) -> bool {
    match manifest.codex_transport.as_deref() {
        None => true,
        Some(recorded) => recorded == active_transport,
    }
}

/// Resolve the session/thread id passed to Codex native resume.
/// Exec and app-server thread ids are not interchangeable across transport.
fn codex_resume_session_id(
    manifest: Option<&crate::models::SessionManifest>,
    last_turn_session: Option<&str>,
    provider_name: &str,
    provider_is_codex: bool,
    active_transport: &str,
) -> Option<String> {
    if !provider_is_codex {
        return None;
    }
    let Some(m) = manifest else {
        return last_turn_session.map(str::to_string);
    };
    if m.provider != provider_name {
        return None;
    }
    if !codex_transport_aligns(m, active_transport) {
        return None;
    }
    m.codex_thread_id
        .as_deref()
        .or(last_turn_session)
        .map(str::to_string)
}

/// Prior Codex context exists for replay when native resume is skipped.
fn codex_had_prior_context(
    manifest: Option<&crate::models::SessionManifest>,
    last_turn_session: Option<&str>,
) -> bool {
    manifest
        .and_then(|m| m.codex_thread_id.as_deref())
        .is_some()
        || last_turn_session.is_some()
}

/// Update manifest Codex provenance after a successful follow-up.
fn apply_codex_followup_manifest(
    manifest: &mut crate::models::SessionManifest,
    result: &crate::providers::FollowupResult,
    active_transport: &str,
) {
    if result.resumed {
        manifest.resume_count = manifest.resume_count.saturating_add(1);
        manifest.last_resumed_at = Some(chrono::Utc::now());
    }
    if active_transport != "app-server" {
        return;
    }
    if let Some(tid) = result.session_id.as_ref() {
        manifest.codex_thread_id = Some(tid.clone());
        manifest.codex_capture_method = Some("app_server_thread_id".into());
    } else if result.resumed {
        // Resume succeeded but no id echoed; keep existing canonical thread.
        manifest.codex_capture_method = Some("app_server_thread_id".into());
    }
}

/// Append a follow-up turn pair (analyst question + provider response)
/// to the conversation log under `ticket_dir`. Does NOT mutate the
/// five-markdown folder — that only happens on /revise (see
/// `investigate_one_structured` with `followup_mode=true`, Task 12).
///
/// Acquires the per-ticket lock for both writes (analyst turn + provider
/// turn). The caller is expected to have already validated `prompt` (e.g.
/// rendered it from analyst input + attached evidence bodies).
#[allow(clippy::too_many_arguments)]
pub async fn followup_turn(
    ticket_dir: &std::path::Path,
    ticket_id: &str,
    prompt: &str,
    system_prompt: &str,
    model: &str,
    attachments: &[crate::models::Attachment],
    provider: &dyn crate::providers::LlmProvider,
    reporter: Option<&dyn crate::chat::ChatPhaseReporter>,
) -> Result<crate::providers::FollowupResult, PipelineError> {
    followup_turn_with_cancel(
        ticket_dir,
        ticket_id,
        prompt,
        system_prompt,
        model,
        attachments,
        provider,
        reporter,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn followup_turn_with_cancel(
    ticket_dir: &std::path::Path,
    ticket_id: &str,
    prompt: &str,
    system_prompt: &str,
    model: &str,
    attachments: &[crate::models::Attachment],
    provider: &dyn crate::providers::LlmProvider,
    reporter: Option<&dyn crate::chat::ChatPhaseReporter>,
    cancel_rx: Option<tokio::sync::watch::Receiver<Option<crate::providers::FollowupCancel>>>,
) -> Result<crate::providers::FollowupResult, PipelineError> {
    // Acquire the per-ticket lock BEFORE reading conversation state. Reading
    // outside the lock allows a concurrent writer to append between our read
    // and our lock-acquire, yielding a stale `next_turn` and a duplicate turn
    // number on append.
    let conv = chat::conversation_jsonl_path(ticket_dir);
    let session_dir = chat::session_dir(ticket_dir);
    let _guard =
        chat::acquire_session_lock(&session_dir, Duration::from_secs(5)).map_err(|e| match e {
            crate::chat::ChatError::LockContention { lock_path } => {
                FollowupError::LockContention(lock_path)
            }
            other => FollowupError::Chat(other),
        })?;

    // Read existing turns under the lock to determine next turn number + session id.
    let outcome = chat::parse_conversation_jsonl(&conv).map_err(FollowupError::from)?;
    // Filter to Codex turns only, then take the most recent one's session_id.
    // Scanning all turns would pick up a stale session_id from an older codex
    // turn when the newest codex turn has session_id: None — that stale id
    // would trigger a spurious session_lost on the next followup.
    let last_codex_session = outcome
        .turns
        .iter()
        .rev()
        .find(|t| matches!(t.turn_kind, crate::models::TurnKind::Codex))
        .and_then(|t| t.session_id.clone());
    let next_turn = outcome.turns.iter().map(|t| t.turn).max().unwrap_or(0) + 1;
    let provider_is_codex = provider.name() == "codex";
    let active_codex_transport = crate::providers::active_codex_transport(provider);
    let manifest = chat::read_session_manifest_opt(ticket_dir).ok().flatten();
    let resume_session_id = active_codex_transport.and_then(|transport| {
        codex_resume_session_id(
            manifest.as_ref(),
            last_codex_session.as_deref(),
            provider.name(),
            provider_is_codex,
            transport,
        )
    });
    let had_prior_codex_context = provider_is_codex
        && codex_had_prior_context(manifest.as_ref(), last_codex_session.as_deref());
    let should_replay_context =
        resume_session_id.is_some() || (had_prior_codex_context && resume_session_id.is_none());

    // Apply PII redaction at the LLM boundary (spec § 7.1g, § 9.3).
    // The redactor scrubs caller PII (phones, addresses, GPS coords);
    // operational identifiers (Call-IDs, station codes, CNC UUIDs,
    // site names) are preserved.
    let (redacted_prompt, _redaction_counts) = crate::redact::redact(prompt);

    // Seed ticket context into the system prompt (#22). Without this the
    // default Unleash provider (stateless HTTP) and the first Codex turn
    // answered with zero knowledge of the ticket or the fork decision. The
    // helper is internally PII-redacted and length-capped.
    //
    // When a prior Codex session exists, a resume is about to be attempted;
    // if it fails ("no rollout found for thread id") the codex provider
    // silently restarts a fresh `codex exec` with no server-side history
    // (#23). To make that fallback context-aware we additionally fold a
    // bounded replay of recent turns into the system prompt. Codex prepends
    // the system prompt on *both* the resume and the fresh-exec path, so
    // seeding it here covers the session-loss case without a signature
    // change. (The analyst-facing System warning turn is still appended
    // below — that behavior is unchanged.)
    let combined_system_prompt = {
        let mut parts: Vec<String> = Vec::new();
        // Redact caller system_prompt at the LLM boundary: `followup_turn` is
        // `pub`, so any future non-empty caller string must be scrubbed
        // regardless of caller convention.
        if !system_prompt.trim().is_empty() {
            let (redacted_sys, _) = crate::redact::redact(system_prompt);
            parts.push(redacted_sys);
        }
        if let Some(ctx) = chat::build_ticket_context_preamble(ticket_dir) {
            parts.push(ctx);
        }
        if should_replay_context {
            if let Some(replay) =
                chat::build_conversation_replay(&outcome.turns, chat::CONVERSATION_REPLAY_TURNS)
            {
                parts.push(replay);
            }
        }
        // Apply an outer cap on the fully assembled prompt so that the
        // preamble + replay + caller string cannot exceed the combined ceiling
        // even when all three components are at their individual limits.
        let assembled = parts.join("\n\n");
        chat::truncate_on_boundary(
            &assembled,
            chat::COMBINED_SYSTEM_PROMPT_CAP_BYTES,
            "\n\n[system prompt truncated]",
        )
    };

    if let Some(reporter) = reporter {
        reporter.phase(crate::chat::ChatStage::ContextAssembled);
    }

    // Call provider
    if let Some(reporter) = reporter {
        if resume_session_id.is_some() {
            reporter.phase(crate::chat::ChatStage::SessionResumeAttempt);
        }
        reporter.phase(crate::chat::ChatStage::ProviderAwait);
    }
    let started = std::time::Instant::now();
    let result = provider
        .followup_with_cancel(
            resume_session_id.as_deref(),
            &redacted_prompt,
            &combined_system_prompt,
            model,
            attachments,
            cancel_rx,
        )
        .await
        .map_err(FollowupError::Provider)?;
    let elapsed_s = started.elapsed().as_secs_f64();

    if let Some(reporter) = reporter {
        reporter.phase(crate::chat::ChatStage::ResponseParsed);
    }

    // Detect codex session-lost fallback: we attempted a resume (prior session
    // existed) but the provider did NOT resume — it started fresh without the
    // prior turn context. Insert a System turn BEFORE the codex turn so the
    // analyst knows the model has amnesia and can restate relevant facts.
    let session_lost = resume_session_id.is_some() && !result.resumed;
    let codex_turn_number = if session_lost {
        let system_turn = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: ticket_id.to_string(),
            turn: next_turn,
            turn_kind: crate::models::TurnKind::System,
            ts: chrono::Utc::now(),
            author: None,
            body: "Codex resume failed — continuing in a fresh session. Prior turn context is no longer available to the model; restate relevant facts in your next question if needed.".to_string(),
            evidence: vec![],
            provider: None,
            model: None,
            tokens_in: None,
            tokens_out: None,
            elapsed_s: None,
            session_id: None,
            resumed: None,
            action: Some(SESSION_LOST_ACTION.to_string()),
            outcome: None,
            drove_revision_from_turns: None,
            diff: None,
        };
        chat::append_turn(&conv, &system_turn).map_err(FollowupError::Chat)?;
        next_turn + 1
    } else {
        next_turn
    };

    // Append the provider turn
    let provider_turn = crate::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: ticket_id.to_string(),
        turn: codex_turn_number,
        turn_kind: crate::models::TurnKind::Codex,
        ts: chrono::Utc::now(),
        author: None,
        body: result.text.clone(),
        evidence: vec![],
        provider: Some(provider.name().to_string()),
        model: Some(model.to_string()),
        tokens_in: result.tokens_in,
        tokens_out: result.tokens_out,
        elapsed_s: Some(elapsed_s),
        session_id: result.session_id.clone(),
        resumed: Some(result.resumed),
        action: None,
        outcome: None,
        drove_revision_from_turns: None,
        diff: None,
    };
    chat::append_turn(&conv, &provider_turn).map_err(FollowupError::Chat)?;

    // Re-render markdown
    let parsed = chat::parse_conversation_jsonl(&conv).map_err(FollowupError::from)?;
    chat::write_conversation_md(
        &chat::conversation_md_path(ticket_dir),
        &parsed.turns,
        ticket_id,
    )
    .map_err(FollowupError::from)?;

    if let Some(reporter) = reporter {
        reporter.phase(crate::chat::ChatStage::Saved);
    }

    // Update manifest (best-effort — failure here is logged but not fatal)
    if let Ok(Some(mut m)) = chat::read_session_manifest_opt(ticket_dir) {
        let mut wrote = false;
        if provider_is_codex {
            if let Some(transport) = active_codex_transport {
                if codex_transport_aligns(&m, transport) {
                    apply_codex_followup_manifest(&mut m, &result, transport);
                    let _ = chat::write_session_manifest(ticket_dir, &m);
                    wrote = true;
                }
            }
        }
        if !wrote && result.resumed {
            // Non-codex or cross-transport: only bump resume counters.
            m.resume_count = m.resume_count.saturating_add(1);
            m.last_resumed_at = Some(chrono::Utc::now());
            let _ = chat::write_session_manifest(ticket_dir, &m);
        }
    } else {
        // First follow-up: create the manifest
        let mut m = crate::models::SessionManifest {
            version: 1,
            provider: provider.name().to_string(),
            model: model.to_string(),
            created_at: chrono::Utc::now(),
            last_resumed_at: None,
            resume_count: 0,
            codex_thread_id: None,
            codex_transport: active_codex_transport.map(str::to_string),
            codex_capture_method: None,
        };
        if let Some(transport) = active_codex_transport {
            apply_codex_followup_manifest(&mut m, &result, transport);
        }
        let _ = chat::write_session_manifest(ticket_dir, &m);
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest(
        codex_transport: &str,
        codex_thread_id: Option<&str>,
    ) -> crate::models::SessionManifest {
        crate::models::SessionManifest {
            version: 1,
            provider: "codex".into(),
            model: "gpt-5.5".into(),
            created_at: chrono::Utc::now(),
            last_resumed_at: None,
            resume_count: 0,
            codex_thread_id: codex_thread_id.map(str::to_string),
            codex_transport: Some(codex_transport.into()),
            codex_capture_method: None,
        }
    }

    struct CaptureProvider {
        resumed_session: std::sync::Mutex<Option<String>>,
        response_session_id: Option<String>,
        response_resumed: bool,
    }

    impl crate::providers::LlmProvider for CaptureProvider {
        fn name(&self) -> &'static str {
            "codex"
        }

        fn codex_transport(&self) -> Option<&'static str> {
            Some("app-server")
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
            Box::pin(async { unreachable!() })
        }

        fn followup<'a>(
            &'a self,
            session_id: Option<&'a str>,
            _prompt: &'a str,
            _system_prompt: &'a str,
            _model: &'a str,
            _attachments: &'a [crate::models::Attachment],
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
            Box::pin(async move {
                *self.resumed_session.lock().unwrap() = session_id.map(str::to_string);
                Ok(crate::providers::FollowupResult {
                    text: "answer".into(),
                    session_id: self.response_session_id.clone(),
                    resumed: self.response_resumed,
                    ..Default::default()
                })
            })
        }
    }

    #[test]
    fn codex_resume_session_id_prefers_manifest_thread_when_aligned() {
        let m = sample_manifest("app-server", Some("manifest-thread"));
        let sid =
            codex_resume_session_id(Some(&m), Some("turn-thread"), "codex", true, "app-server");
        assert_eq!(sid.as_deref(), Some("manifest-thread"));
    }

    #[test]
    fn codex_resume_session_id_skips_on_transport_mismatch() {
        let m = sample_manifest("exec", Some("stale-thread"));
        assert!(codex_resume_session_id(
            Some(&m),
            Some("turn-thread"),
            "codex",
            true,
            "app-server"
        )
        .is_none());
    }

    #[test]
    fn apply_codex_followup_manifest_sets_thread_and_capture_method() {
        let mut m = sample_manifest("app-server", None);
        let result = crate::providers::FollowupResult {
            text: "ok".into(),
            session_id: Some("thread-new".into()),
            resumed: true,
            ..Default::default()
        };
        apply_codex_followup_manifest(&mut m, &result, "app-server");
        assert_eq!(m.codex_thread_id.as_deref(), Some("thread-new"));
        assert_eq!(
            m.codex_capture_method.as_deref(),
            Some("app_server_thread_id")
        );
        assert_eq!(m.resume_count, 1);
    }

    #[tokio::test]
    async fn followup_turn_persists_app_server_manifest_on_first_followup() {
        use crate::chat;

        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("45002");
        std::fs::create_dir_all(chat::session_dir(&ticket_dir)).unwrap();

        let provider = CaptureProvider {
            resumed_session: std::sync::Mutex::new(None),
            response_session_id: Some("thread-from-provider".into()),
            response_resumed: false,
        };
        followup_turn(
            &ticket_dir,
            "45002",
            "next?",
            "",
            "gpt-5.5",
            &[],
            &provider,
            None,
        )
        .await
        .unwrap();

        assert!(provider.resumed_session.lock().unwrap().is_none());
        let m = chat::read_session_manifest(&ticket_dir).unwrap();
        assert_eq!(m.provider, "codex");
        assert_eq!(m.codex_transport.as_deref(), Some("app-server"));
        assert_eq!(m.codex_thread_id.as_deref(), Some("thread-from-provider"));
        assert_eq!(
            m.codex_capture_method.as_deref(),
            Some("app_server_thread_id")
        );
    }

    #[tokio::test]
    async fn followup_turn_skips_native_resume_on_transport_mismatch() {
        use crate::chat;

        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("45003");
        std::fs::create_dir_all(chat::session_dir(&ticket_dir)).unwrap();

        let manifest = sample_manifest("exec", Some("cross-transport-thread"));
        chat::write_session_manifest(&ticket_dir, &manifest).unwrap();

        let prior = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "45003".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Codex,
            ts: chrono::Utc::now(),
            author: None,
            body: "prior".into(),
            evidence: vec![],
            provider: Some("codex".into()),
            model: Some("gpt-5.5".into()),
            tokens_in: None,
            tokens_out: None,
            elapsed_s: None,
            session_id: Some("turn-only-thread".into()),
            resumed: Some(false),
            action: None,
            outcome: None,
            drove_revision_from_turns: None,
            diff: None,
        };
        chat::append_turn(&chat::conversation_jsonl_path(&ticket_dir), &prior).unwrap();

        let provider = CaptureProvider {
            resumed_session: std::sync::Mutex::new(None),
            response_session_id: Some("fresh-thread".into()),
            response_resumed: false,
        };
        followup_turn(
            &ticket_dir,
            "45003",
            "continue",
            "",
            "gpt-5.5",
            &[],
            &provider,
            None,
        )
        .await
        .unwrap();

        assert!(provider.resumed_session.lock().unwrap().is_none());
        let m = chat::read_session_manifest(&ticket_dir).unwrap();
        assert_eq!(m.codex_thread_id.as_deref(), Some("cross-transport-thread"));
        assert_eq!(m.codex_transport.as_deref(), Some("exec"));
    }
}
