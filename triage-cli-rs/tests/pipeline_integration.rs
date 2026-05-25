//! Async integration tests moved from pipeline.rs #[cfg(test)] block.

use triage_cli::fixture::{FixtureDatadogClient, FixtureLoader};
use triage_cli::paths::TRIAGE_HOME_ENV;
use triage_cli::pipeline::{
    followup_turn, investigate_one_structured, revise, InvestigateOptions, MetricValue,
    MetricsReporter, PipelineError, SilentReporter, StructuredInvestigation, SESSION_LOST_ACTION,
};
use triage_cli::playbook::Rubric;
use triage_cli::ticket_folder::TicketFolderError;

struct FixtureEnvScope {
    _memory: triage_cli::memory::MemoryEnvScope,
    prev_home: Option<String>,
}

impl FixtureEnvScope {
    fn new(home: &std::path::Path, tickets_root: &std::path::Path) -> Self {
        std::fs::create_dir_all(home.join("data")).expect("fixture test home data dir");
        let memory_md = home.join("MEMORY.md");
        let memory_db = home.join("data/memory.db");
        let memory = triage_cli::memory::MemoryEnvScope::new_with_tickets_root(
            &memory_md,
            &memory_db,
            Some(tickets_root),
        );
        let prev_home = std::env::var(TRIAGE_HOME_ENV).ok();
        std::env::set_var(TRIAGE_HOME_ENV, home);
        Self {
            _memory: memory,
            prev_home,
        }
    }
}

impl Drop for FixtureEnvScope {
    fn drop(&mut self) {
        match &self.prev_home {
            Some(value) => std::env::set_var(TRIAGE_HOME_ENV, value),
            None => std::env::remove_var(TRIAGE_HOME_ENV),
        }
    }
}

/// Run the pipeline against the audio-drop fixture (ticket #55001) using
/// `no_llm: true` so no network calls are made. Returns the pipeline result.
///
/// Acquires the shared memory env-guard so that this test is serialised
/// with every `memory::tests::*` test that also touches
/// `TRIAGE_MEMORY_MD` / `TRIAGE_MEMORY_DB`.  All three process-global
/// env vars are overridden to paths inside `tickets_root` and restored
/// on return.
async fn run_fixture_pipeline(
    tickets_root: &std::path::Path,
) -> Result<StructuredInvestigation, PipelineError> {
    // Point the fixture loader at the crate's bundled fixtures directory.
    let fixtures_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let loader =
        FixtureLoader::new(fixtures_dir.join("audio-drop")).expect("audio-drop fixture must exist");
    let ticket = loader.load_ticket().expect("fixture ticket.json");
    let logs = loader
        .load_datadog_logs()
        .expect("fixture datadog-logs.json");
    let memory_hits = loader.load_memory_hits().expect("fixture memory-hits.json");

    let mut session = triage_cli::investigation::create_session(ticket.clone());
    let fixture_dd = FixtureDatadogClient::new(logs);
    let opts = InvestigateOptions {
        no_llm: true,
        memory_hits_override: Some(memory_hits),
        force: true, // avoid soft-lock conflicts between parallel test runs
        followup_mode: false,
        ..InvestigateOptions::defaults()
    };
    let rubric = Rubric::load().expect("embedded rubric must parse");
    let reporter = SilentReporter;

    // Override TRIAGE_TICKETS_ROOT, TRIAGE_MEMORY_MD, and TRIAGE_MEMORY_DB
    // to paths inside this test's tempdir, and hold the process-wide env
    // mutex for the duration so we don't race with memory::tests::*.
    let memory_md = tickets_root.join("MEMORY.md");
    let memory_db = tickets_root.join("data/memory.db");
    let _env = triage_cli::memory::MemoryEnvScope::new_with_tickets_root(
        &memory_md,
        &memory_db,
        Some(tickets_root),
    );

    investigate_one_structured(
        ticket,
        &mut session,
        None,
        Some(&fixture_dd as &dyn triage_cli::datadog::DatadogSource),
        &rubric,
        &reporter,
        &opts,
    )
    .await
}

#[tokio::test]
async fn investigate_writes_base_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = run_fixture_pipeline(dir.path()).await;
    assert!(
        outcome.is_ok(),
        "fixture pipeline failed: {:?}",
        outcome.err()
    );

    // Ticket #55001 is the audio-drop fixture id
    let ticket_dir = dir.path().join("55001");

    // Both snapshots must exist after a successful non-followup run
    assert!(
        ticket_dir.join(".session/base-ticket.json").exists(),
        "base-ticket.json not written"
    );
    assert!(
        ticket_dir
            .join(".session/base-evidence-manifest.json")
            .exists(),
        "base-evidence-manifest.json not written"
    );

    // Round-trip the snapshots to confirm they are valid JSON
    let bt =
        triage_cli::chat::read_base_ticket(&ticket_dir).expect("base-ticket.json must round-trip");
    let _ = bt.id; // round-trip asserted by successful read

    let bem = triage_cli::chat::read_base_evidence_manifest(&ticket_dir)
        .expect("base-evidence-manifest.json must round-trip");
    let _ = bem.ticket_id; // round-trip asserted by successful read
}

#[tokio::test]
async fn fixture_datadog_logs_survive_isolated_triage_home() {
    let home = tempfile::tempdir().unwrap();
    let tickets_root = home.path().join("Tickets");
    std::fs::create_dir_all(&tickets_root).unwrap();
    let _env = FixtureEnvScope::new(home.path(), &tickets_root);

    let fixtures_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    let loader =
        FixtureLoader::new(fixtures_dir.join("audio-drop")).expect("audio-drop fixture must exist");
    let ticket = loader.load_ticket().expect("fixture ticket.json");
    let logs = loader
        .load_datadog_logs()
        .expect("fixture datadog-logs.json");
    let memory_hits = loader.load_memory_hits().expect("fixture memory-hits.json");

    let mut session = triage_cli::investigation::create_session(ticket.clone());
    let fixture_dd = FixtureDatadogClient::new(logs);
    let opts = InvestigateOptions {
        no_llm: true,
        memory_hits_override: Some(memory_hits),
        force: true,
        followup_mode: false,
        allow_unscoped_fixture_logs: true,
        ..InvestigateOptions::defaults()
    };
    let rubric = Rubric::load().expect("embedded rubric must parse");
    let reporter = MetricsReporter::new(Box::new(SilentReporter));

    let outcome = investigate_one_structured(
        ticket,
        &mut session,
        None,
        Some(&fixture_dd as &dyn triage_cli::datadog::DatadogSource),
        &rubric,
        &reporter,
        &opts,
    )
    .await;
    assert!(
        outcome.is_ok(),
        "fixture pipeline failed: {:?}",
        outcome.err()
    );

    let datadog_lines = reporter
        .named_metrics()
        .into_iter()
        .find_map(|(key, value)| match (key.as_str(), value) {
            ("evidence.datadog_lines", MetricValue::Int(lines)) => Some(lines),
            _ => None,
        })
        .expect("datadog metric should be recorded");
    assert_eq!(
        datadog_lines, 8,
        "fixture logs should survive isolated TRIAGE_HOME runs"
    );
}

#[tokio::test]
async fn followup_turn_appends_to_conversation_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("44776");
    std::fs::create_dir_all(&ticket_dir).unwrap();

    // Seed an analyst turn-001
    let analyst = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44776".into(),
        turn: 1,
        turn_kind: triage_cli::models::TurnKind::Analyst,
        ts: chrono::Utc::now(),
        author: Some("enrique".into()),
        body: "what's up?".into(),
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
    let conv = triage_cli::chat::conversation_jsonl_path(&ticket_dir);
    {
        let _guard = triage_cli::chat::acquire_session_lock(
            &triage_cli::chat::session_dir(&ticket_dir),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        triage_cli::chat::append_turn(&conv, &analyst).unwrap();
    }

    // Fake provider that returns canned text
    struct FakeProvider;
    impl triage_cli::providers::LlmProvider for FakeProvider {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn complete<'a>(
            &'a self,
            _prompt: &'a str,
            _sys: &'a str,
            _model: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            triage_cli::providers::CompletionResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                Ok(triage_cli::providers::CompletionResult {
                    text: "fake codex reply".into(),
                    tokens_in: Some(100),
                    tokens_out: Some(50),
                })
            })
        }
    }

    let provider: Box<dyn triage_cli::providers::LlmProvider> = Box::new(FakeProvider);
    let result = followup_turn(
        &ticket_dir,
        "44776",
        "follow-up question",
        "system",
        "fake-model",
        &[],
        provider.as_ref(),
        None,
    )
    .await
    .unwrap();

    assert!(result.text.contains("fake codex reply"));

    // Conversation now has turn-001 analyst + turn-002 codex
    let parsed = triage_cli::chat::parse_conversation_jsonl(&conv).unwrap();
    assert_eq!(parsed.turns.len(), 2);
    assert!(matches!(
        parsed.turns[1].turn_kind,
        triage_cli::models::TurnKind::Codex
    ));
}

#[tokio::test]
async fn followup_turn_inserts_system_note_when_codex_session_lost() {
    use triage_cli::chat;
    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("44999");
    std::fs::create_dir_all(&ticket_dir).unwrap();

    // Seed conversation with an analyst turn AND a codex turn that has a
    // session_id — this is what triggers the resume-attempt path.
    let analyst = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44999".into(),
        turn: 1,
        turn_kind: triage_cli::models::TurnKind::Analyst,
        ts: chrono::Utc::now(),
        author: Some("enrique".into()),
        body: "first question".into(),
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
    let codex = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44999".into(),
        turn: 2,
        turn_kind: triage_cli::models::TurnKind::Codex,
        ts: chrono::Utc::now(),
        author: None,
        body: "first answer".into(),
        evidence: vec![],
        provider: Some("codex".into()),
        model: Some("gpt-5.5".into()),
        tokens_in: None,
        tokens_out: None,
        elapsed_s: None,
        session_id: Some("01HOLD000000".into()),
        resumed: Some(false),
        action: None,
        outcome: None,
        drove_revision_from_turns: None,
        diff: None,
    };
    let conv = chat::conversation_jsonl_path(&ticket_dir);
    {
        let _g = chat::acquire_session_lock(
            &chat::session_dir(&ticket_dir),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        chat::append_turn(&conv, &analyst).unwrap();
        chat::append_turn(&conv, &codex).unwrap();
    }

    // FakeProvider that mimics the codex session-lost fallback: resumed=false
    // with a freshly-issued session_id.
    struct LostSessionProvider;
    impl triage_cli::providers::LlmProvider for LostSessionProvider {
        fn name(&self) -> &'static str {
            "codex"
        }
        fn complete<'a>(
            &'a self,
            _prompt: &'a str,
            _sys: &'a str,
            _model: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            triage_cli::providers::CompletionResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                Ok(triage_cli::providers::CompletionResult {
                    text: "fresh response".into(),
                    tokens_in: None,
                    tokens_out: None,
                })
            })
        }
        fn followup<'a>(
            &'a self,
            _session_id: Option<&'a str>,
            _prompt: &'a str,
            _sys: &'a str,
            _model: &'a str,
            _attachments: &'a [triage_cli::models::Attachment],
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            triage_cli::providers::FollowupResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                Ok(triage_cli::providers::FollowupResult {
                    text: "fresh response".into(),
                    tokens_in: None,
                    tokens_out: None,
                    session_id: Some("01HFRESH00000".into()),
                    resumed: false,
                })
            })
        }
    }

    let provider: Box<dyn triage_cli::providers::LlmProvider> = Box::new(LostSessionProvider);
    followup_turn(
        &ticket_dir,
        "44999",
        "follow-up after session lost",
        "system",
        "gpt-5.5",
        &[],
        provider.as_ref(),
        None,
    )
    .await
    .unwrap();

    let parsed = chat::parse_conversation_jsonl(&conv).unwrap();
    // Expect: analyst(1), codex(2), system-session-lost(3), codex(4)
    assert_eq!(
        parsed.turns.len(),
        4,
        "expected analyst+codex+system+codex; got {:?}",
        parsed
            .turns
            .iter()
            .map(|t| (t.turn, &t.turn_kind, t.action.clone()))
            .collect::<Vec<_>>()
    );
    let system_turn = parsed
        .turns
        .iter()
        .find(|t| matches!(t.turn_kind, triage_cli::models::TurnKind::System))
        .expect("system turn must be present");
    assert_eq!(system_turn.action.as_deref(), Some(SESSION_LOST_ACTION));
    // The system turn must precede the new codex turn in the JSONL.
    let positions: Vec<(u32, &triage_cli::models::TurnKind)> = parsed
        .turns
        .iter()
        .map(|t| (t.turn, &t.turn_kind))
        .collect();
    let sys_idx = positions
        .iter()
        .position(|(_, k)| matches!(k, triage_cli::models::TurnKind::System))
        .unwrap();
    let new_codex_idx = positions
        .iter()
        .rposition(|(_, k)| matches!(k, triage_cli::models::TurnKind::Codex))
        .unwrap();
    assert!(
        sys_idx < new_codex_idx,
        "system turn must precede new codex turn in JSONL order"
    );
}

#[tokio::test]
async fn followup_turn_no_system_note_on_first_followup() {
    // No prior codex session: provider returns resumed=false with a fresh
    // session_id, which is normal first-followup behavior — NOT session-lost.
    // The function must NOT insert a system "session_lost" turn here.
    use triage_cli::chat;
    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("44998");
    std::fs::create_dir_all(&ticket_dir).unwrap();

    // Seed only an analyst turn — no prior codex session_id exists.
    let analyst = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44998".into(),
        turn: 1,
        turn_kind: triage_cli::models::TurnKind::Analyst,
        ts: chrono::Utc::now(),
        author: Some("enrique".into()),
        body: "initial question".into(),
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
    let conv = chat::conversation_jsonl_path(&ticket_dir);
    {
        let _g = chat::acquire_session_lock(
            &chat::session_dir(&ticket_dir),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        chat::append_turn(&conv, &analyst).unwrap();
    }

    // Provider returns resumed=false with a new session_id — normal first-call.
    struct FirstFollowupProvider;
    impl triage_cli::providers::LlmProvider for FirstFollowupProvider {
        fn name(&self) -> &'static str {
            "fake-first"
        }
        fn complete<'a>(
            &'a self,
            _prompt: &'a str,
            _sys: &'a str,
            _model: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            triage_cli::providers::CompletionResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                Ok(triage_cli::providers::CompletionResult {
                    text: "first codex reply".into(),
                    tokens_in: None,
                    tokens_out: None,
                })
            })
        }
        fn followup<'a>(
            &'a self,
            _session_id: Option<&'a str>,
            _prompt: &'a str,
            _sys: &'a str,
            _model: &'a str,
            _attachments: &'a [triage_cli::models::Attachment],
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            triage_cli::providers::FollowupResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                Ok(triage_cli::providers::FollowupResult {
                    text: "first codex reply".into(),
                    tokens_in: None,
                    tokens_out: None,
                    session_id: Some("01HFIRST00000".into()),
                    resumed: false,
                })
            })
        }
    }

    let provider: Box<dyn triage_cli::providers::LlmProvider> = Box::new(FirstFollowupProvider);
    followup_turn(
        &ticket_dir,
        "44998",
        "first follow-up question",
        "system",
        "gpt-5.5",
        &[],
        provider.as_ref(),
        None,
    )
    .await
    .unwrap();

    let parsed = chat::parse_conversation_jsonl(&conv).unwrap();
    // Expect: analyst(1), codex(2) — no system turn
    assert_eq!(
        parsed.turns.len(),
        2,
        "expected analyst+codex only (no session_lost system turn); got {:?}",
        parsed
            .turns
            .iter()
            .map(|t| (t.turn, &t.turn_kind, t.action.clone()))
            .collect::<Vec<_>>()
    );
    assert!(
        !parsed
            .turns
            .iter()
            .any(|t| matches!(t.turn_kind, triage_cli::models::TurnKind::System)),
        "no System turn expected on first follow-up"
    );
}

#[tokio::test]
async fn followup_turn_no_system_note_when_latest_codex_session_id_is_none() {
    // Regression: when the most-recent codex turn has session_id=None,
    // last_codex_session must be None and NO resume is attempted. Because
    // no resume was attempted, the provider's resumed=false response must
    // NOT be interpreted as a session-lost event, and no System turn
    // should be inserted.
    //
    // Turn layout seeded:
    //   turn-1  Analyst
    //   turn-2  Codex  session_id="01HOLD000000"  (older; must be ignored)
    //   turn-3  Analyst
    //   turn-4  Codex  session_id=None             (most recent codex)
    //   turn-5  Analyst
    use triage_cli::chat;
    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("44996");
    std::fs::create_dir_all(&ticket_dir).unwrap();

    let make_analyst = |turn: u32, body: &str| triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44996".into(),
        turn,
        turn_kind: triage_cli::models::TurnKind::Analyst,
        ts: chrono::Utc::now(),
        author: Some("enrique".into()),
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
    let make_codex = |turn: u32, sid: Option<&str>| triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44996".into(),
        turn,
        turn_kind: triage_cli::models::TurnKind::Codex,
        ts: chrono::Utc::now(),
        author: None,
        body: "codex answer".into(),
        evidence: vec![],
        provider: Some("codex".into()),
        model: Some("gpt-5.5".into()),
        tokens_in: None,
        tokens_out: None,
        elapsed_s: None,
        session_id: sid.map(str::to_string),
        resumed: Some(false),
        action: None,
        outcome: None,
        drove_revision_from_turns: None,
        diff: None,
    };

    let conv = chat::conversation_jsonl_path(&ticket_dir);
    {
        let _g = chat::acquire_session_lock(
            &chat::session_dir(&ticket_dir),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        chat::append_turn(&conv, &make_analyst(1, "question one")).unwrap();
        chat::append_turn(&conv, &make_codex(2, Some("01HOLD000000"))).unwrap();
        chat::append_turn(&conv, &make_analyst(3, "question two")).unwrap();
        chat::append_turn(&conv, &make_codex(4, None)).unwrap(); // most recent codex: no session_id
        chat::append_turn(&conv, &make_analyst(5, "question three")).unwrap();
    }

    // Provider always returns resumed=false; if last_codex_session were
    // mistakenly set to "01HOLD000000" this would be interpreted as
    // session-lost and a System turn would be inserted.
    struct ResumedFalseProvider;
    impl triage_cli::providers::LlmProvider for ResumedFalseProvider {
        fn name(&self) -> &'static str {
            "fake-resumed-false"
        }
        fn complete<'a>(
            &'a self,
            _prompt: &'a str,
            _sys: &'a str,
            _model: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            triage_cli::providers::CompletionResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                Ok(triage_cli::providers::CompletionResult {
                    text: "answer".into(),
                    tokens_in: None,
                    tokens_out: None,
                })
            })
        }
        fn followup<'a>(
            &'a self,
            _session_id: Option<&'a str>,
            _prompt: &'a str,
            _sys: &'a str,
            _model: &'a str,
            _attachments: &'a [triage_cli::models::Attachment],
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            triage_cli::providers::FollowupResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                Ok(triage_cli::providers::FollowupResult {
                    text: "fresh answer".into(),
                    tokens_in: None,
                    tokens_out: None,
                    session_id: Some("01HNEW000001".into()),
                    resumed: false,
                })
            })
        }
    }

    let provider: Box<dyn triage_cli::providers::LlmProvider> = Box::new(ResumedFalseProvider);
    followup_turn(
        &ticket_dir,
        "44996",
        "follow-up after codex with no session_id",
        "system",
        "gpt-5.5",
        &[],
        provider.as_ref(),
        None,
    )
    .await
    .unwrap();

    let parsed = chat::parse_conversation_jsonl(&conv).unwrap();
    // Expect: analyst(1)+codex(2)+analyst(3)+codex(4)+analyst(5)+codex(6)
    // — no System turn because last_codex_session was None.
    assert!(
        !parsed
            .turns
            .iter()
            .any(|t| matches!(t.turn_kind, triage_cli::models::TurnKind::System)),
        "no System turn expected when most recent codex had session_id=None; turns: {:?}",
        parsed
            .turns
            .iter()
            .map(|t| (t.turn, &t.turn_kind, t.action.as_deref()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        parsed.turns.len(),
        6,
        "expected 6 turns (5 seeded + 1 new codex); got {:?}",
        parsed
            .turns
            .iter()
            .map(|t| (t.turn, &t.turn_kind))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn followup_turn_applies_pii_redaction() {
    // Verifies that phone numbers in the analyst prompt are scrubbed before
    // reaching the provider (spec § 7.1g, § 9.3).
    use std::sync::Mutex;

    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("44777");
    std::fs::create_dir_all(&ticket_dir).unwrap();

    // Seed an analyst turn-001 so the conversation file exists
    let analyst = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44777".into(),
        turn: 1,
        turn_kind: triage_cli::models::TurnKind::Analyst,
        ts: chrono::Utc::now(),
        author: Some("enrique".into()),
        body: "initial question".into(),
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
    let conv = triage_cli::chat::conversation_jsonl_path(&ticket_dir);
    {
        let _guard = triage_cli::chat::acquire_session_lock(
            &triage_cli::chat::session_dir(&ticket_dir),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        triage_cli::chat::append_turn(&conv, &analyst).unwrap();
    }

    // FakeProvider that records the exact prompt it received
    struct CapturingProvider {
        captured_prompt: Mutex<Option<String>>,
    }
    impl triage_cli::providers::LlmProvider for CapturingProvider {
        fn name(&self) -> &'static str {
            "fake-capturing"
        }
        fn complete<'a>(
            &'a self,
            prompt: &'a str,
            _sys: &'a str,
            _model: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            triage_cli::providers::CompletionResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                *self.captured_prompt.lock().unwrap() = Some(prompt.to_string());
                Ok(triage_cli::providers::CompletionResult {
                    text: "redaction-test reply".into(),
                    tokens_in: Some(5),
                    tokens_out: Some(5),
                })
            })
        }
    }

    // Prompt contains a phone number that should be scrubbed.
    // Uses the same pattern as redact::tests::redacts_phone.
    let prompt_with_pii = "call (555) 123-4567 for the incident report";

    let provider = CapturingProvider {
        captured_prompt: Mutex::new(None),
    };
    followup_turn(
        &ticket_dir,
        "44777",
        prompt_with_pii,
        "system",
        "fake-model",
        &[],
        &provider,
        None,
    )
    .await
    .unwrap();

    let captured = provider
        .captured_prompt
        .lock()
        .unwrap()
        .clone()
        .expect("provider must have been called");

    // The raw phone number must NOT appear in what the provider received.
    assert!(
        !captured.contains("555") || !captured.contains("123-4567"),
        "PII phone number leaked to provider; captured prompt: {captured:?}"
    );
    // The redaction sentinel MUST appear instead.
    assert!(
        captured.contains("<PHONE>"),
        "expected <PHONE> sentinel in redacted prompt; got: {captured:?}"
    );
}

#[tokio::test]
async fn followup_turn_seeds_ticket_context_into_system_prompt() {
    // #22: the chat path passes an empty system prompt; followup_turn
    // must rebuild ticket context (STATE.md / FORK_PACKET.md) and feed
    // it to the provider so Unleash (stateless) and the first Codex
    // turn are not context-blind.
    use std::sync::Mutex;

    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("44778");
    std::fs::create_dir_all(&ticket_dir).unwrap();
    std::fs::write(
        ticket_dir.join("STATE.md"),
        "---\nticket_id: 44778\nfork: A\n---\n",
    )
    .unwrap();
    std::fs::write(
        ticket_dir.join("FORK_PACKET.md"),
        "Recommendation: Fork A — FORK_CONTEXT_SENTINEL.\n",
    )
    .unwrap();

    // Seed an analyst turn-001 so the conversation file exists.
    let analyst = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44778".into(),
        turn: 1,
        turn_kind: triage_cli::models::TurnKind::Analyst,
        ts: chrono::Utc::now(),
        author: Some("enrique".into()),
        body: "does the fork still hold?".into(),
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
    let conv = triage_cli::chat::conversation_jsonl_path(&ticket_dir);
    {
        let _guard = triage_cli::chat::acquire_session_lock(
            &triage_cli::chat::session_dir(&ticket_dir),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        triage_cli::chat::append_turn(&conv, &analyst).unwrap();
    }

    struct SysCapturingProvider {
        captured_system: Mutex<Option<String>>,
    }
    impl triage_cli::providers::LlmProvider for SysCapturingProvider {
        fn name(&self) -> &'static str {
            "fake-sys-capturing"
        }
        fn complete<'a>(
            &'a self,
            _prompt: &'a str,
            system: &'a str,
            _model: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            triage_cli::providers::CompletionResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                *self.captured_system.lock().unwrap() = Some(system.to_string());
                Ok(triage_cli::providers::CompletionResult {
                    text: "context-seed reply".into(),
                    tokens_in: Some(5),
                    tokens_out: Some(5),
                })
            })
        }
    }

    let provider = SysCapturingProvider {
        captured_system: Mutex::new(None),
    };
    // Caller passes an empty system prompt, exactly like the inbox.
    followup_turn(
        &ticket_dir,
        "44778",
        "does the fork still hold?",
        "",
        "fake-model",
        &[],
        &provider,
        None,
    )
    .await
    .unwrap();

    let captured = provider
        .captured_system
        .lock()
        .unwrap()
        .clone()
        .expect("provider must have been called");
    assert!(
        captured.contains("FORK_CONTEXT_SENTINEL"),
        "ticket context was not seeded into system prompt; got: {captured:?}"
    );
    assert!(
        captured.contains("STATE.md") && captured.contains("fork: A"),
        "STATE.md context missing from system prompt; got: {captured:?}"
    );
    // No prior Codex session → no replay block on this first turn.
    assert!(
        !captured.contains("Prior conversation (replayed"),
        "unexpected replay block on first turn: {captured:?}"
    );
}

// ── Issue 1: caller system_prompt redaction ───────────────────────────

#[tokio::test]
async fn followup_turn_redacts_caller_system_prompt() {
    // Verifies that PII in the caller-supplied system_prompt is scrubbed
    // before reaching the provider. The production caller (inbox TUI)
    // passes "", but followup_turn is pub, so the invariant must hold
    // structurally (issue raised in pre-merge review).
    use std::sync::Mutex;

    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("44900");
    std::fs::create_dir_all(&ticket_dir).unwrap();

    // Seed one analyst turn so the conversation file exists.
    let analyst = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44900".into(),
        turn: 1,
        turn_kind: triage_cli::models::TurnKind::Analyst,
        ts: chrono::Utc::now(),
        author: Some("enrique".into()),
        body: "initial question".into(),
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
    let conv = triage_cli::chat::conversation_jsonl_path(&ticket_dir);
    {
        let _guard = triage_cli::chat::acquire_session_lock(
            &triage_cli::chat::session_dir(&ticket_dir),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        triage_cli::chat::append_turn(&conv, &analyst).unwrap();
    }

    struct SysCapture {
        captured_sys: Mutex<Option<String>>,
    }
    impl triage_cli::providers::LlmProvider for SysCapture {
        fn name(&self) -> &'static str {
            "fake-sys-capture"
        }
        fn complete<'a>(
            &'a self,
            _prompt: &'a str,
            system: &'a str,
            _model: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            triage_cli::providers::CompletionResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                *self.captured_sys.lock().unwrap() = Some(system.to_string());
                Ok(triage_cli::providers::CompletionResult {
                    text: "sys-redact reply".into(),
                    tokens_in: None,
                    tokens_out: None,
                })
            })
        }
    }

    let provider = SysCapture {
        captured_sys: Mutex::new(None),
    };

    // Caller passes a non-empty system_prompt containing a phone number
    // (same pattern as redact::tests::redacts_phone).
    let sys_with_pii = "analyst hotline: (555) 123-4567 — do not share";
    followup_turn(
        &ticket_dir,
        "44900",
        "what is the next step?",
        sys_with_pii,
        "fake-model",
        &[],
        &provider,
        None,
    )
    .await
    .unwrap();

    let captured = provider
        .captured_sys
        .lock()
        .unwrap()
        .clone()
        .expect("provider must have been called");

    // The raw phone MUST NOT appear in the system prompt the provider saw.
    assert!(
        !captured.contains("123-4567"),
        "caller system_prompt PII leaked to provider; got: {captured:?}"
    );
    // The redaction sentinel MUST be present.
    assert!(
        captured.contains("<PHONE>"),
        "expected <PHONE> sentinel in redacted system prompt; got: {captured:?}"
    );
}

// ── Issue 2: outer cap on combined system prompt ──────────────────────

#[tokio::test]
async fn followup_turn_combined_system_prompt_is_capped() {
    // Verifies that when all three components (caller prompt + preamble +
    // replay) stack up, the assembled combined_system_prompt is truncated
    // to COMBINED_SYSTEM_PROMPT_CAP_BYTES on a UTF-8 char boundary.
    use std::sync::Mutex;
    use triage_cli::chat;

    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("44901");
    std::fs::create_dir_all(&ticket_dir).unwrap();

    // Write STATE.md and FORK_PACKET.md each much larger than the preamble
    // cap to ensure the preamble component reaches its individual ceiling.
    let fat_content = "X".repeat(chat::CONTEXT_PREAMBLE_CAP_BYTES * 3);
    std::fs::write(ticket_dir.join("STATE.md"), &fat_content).unwrap();
    std::fs::write(ticket_dir.join("FORK_PACKET.md"), &fat_content).unwrap();

    // Seed a prior Codex turn with a session_id to trigger the replay path,
    // and make its body large enough to fill the replay component.
    let analyst = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44901".into(),
        turn: 1,
        turn_kind: triage_cli::models::TurnKind::Analyst,
        ts: chrono::Utc::now(),
        author: None,
        body: "Y".repeat(chat::CONTEXT_PREAMBLE_CAP_BYTES),
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
    let codex = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44901".into(),
        turn: 2,
        turn_kind: triage_cli::models::TurnKind::Codex,
        ts: chrono::Utc::now(),
        author: None,
        body: "Z".repeat(chat::CONTEXT_PREAMBLE_CAP_BYTES),
        evidence: vec![],
        provider: Some("codex".into()),
        model: None,
        tokens_in: None,
        tokens_out: None,
        elapsed_s: None,
        session_id: Some("01HBIG000001".into()),
        resumed: Some(false),
        action: None,
        outcome: None,
        drove_revision_from_turns: None,
        diff: None,
    };
    let conv = chat::conversation_jsonl_path(&ticket_dir);
    {
        let _guard = chat::acquire_session_lock(
            &chat::session_dir(&ticket_dir),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        chat::append_turn(&conv, &analyst).unwrap();
        chat::append_turn(&conv, &codex).unwrap();
    }

    struct SysCap {
        captured_sys: Mutex<Option<String>>,
    }
    impl triage_cli::providers::LlmProvider for SysCap {
        fn name(&self) -> &'static str {
            "fake-sys-cap"
        }
        fn complete<'a>(
            &'a self,
            _p: &'a str,
            sys: &'a str,
            _m: &'a str,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            triage_cli::providers::CompletionResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                *self.captured_sys.lock().unwrap() = Some(sys.to_string());
                Ok(triage_cli::providers::CompletionResult {
                    text: "cap reply".into(),
                    tokens_in: None,
                    tokens_out: None,
                })
            })
        }
        fn followup<'a>(
            &'a self,
            _session_id: Option<&'a str>,
            _prompt: &'a str,
            sys: &'a str,
            _model: &'a str,
            _attachments: &'a [triage_cli::models::Attachment],
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            triage_cli::providers::FollowupResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                *self.captured_sys.lock().unwrap() = Some(sys.to_string());
                Ok(triage_cli::providers::FollowupResult {
                    text: "cap reply".into(),
                    tokens_in: None,
                    tokens_out: None,
                    session_id: None,
                    resumed: false,
                })
            })
        }
    }

    let provider = SysCap {
        captured_sys: Mutex::new(None),
    };

    // Caller prompt is also oversized to stress the combined ceiling.
    // Use the combined cap as the caller prompt size so that caller alone
    // already fills the ceiling; together with even a small preamble it
    // will reliably exceed COMBINED_SYSTEM_PROMPT_CAP_BYTES and trigger
    // the outer truncation.
    let big_caller_sys = "A".repeat(chat::COMBINED_SYSTEM_PROMPT_CAP_BYTES);
    followup_turn(
        &ticket_dir,
        "44901",
        "question",
        &big_caller_sys,
        "fake-model",
        &[],
        &provider,
        None,
    )
    .await
    .unwrap();

    let captured = provider
        .captured_sys
        .lock()
        .unwrap()
        .clone()
        .expect("provider must have been called");

    // The combined system prompt must not exceed the outer cap plus the
    // truncation marker length (allow a small slack for the marker).
    assert!(
        captured.len() <= chat::COMBINED_SYSTEM_PROMPT_CAP_BYTES + 64,
        "combined system prompt exceeded cap: {} bytes (cap={}, slack=64); \
        first 120 bytes: {:?}",
        captured.len(),
        chat::COMBINED_SYSTEM_PROMPT_CAP_BYTES,
        &captured[..captured.len().min(120)],
    );
    // Truncation marker must be present (confirms truncation ran).
    assert!(
        captured.contains("[system prompt truncated]"),
        "expected '[system prompt truncated]' in capped system prompt; got: {captured:?}"
    );
}

#[derive(Default)]
struct RecordingChatReporter {
    stages: std::sync::Mutex<Vec<triage_cli::chat::ChatStage>>,
}

impl triage_cli::chat::ChatPhaseReporter for RecordingChatReporter {
    fn phase(&self, stage: triage_cli::chat::ChatStage) {
        self.stages.lock().unwrap().push(stage);
    }
}

#[tokio::test]
async fn followup_turn_emits_phases_in_order_with_codex_resume() {
    use triage_cli::chat;

    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("45001");
    std::fs::create_dir_all(chat::session_dir(&ticket_dir)).unwrap();

    let prior = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "45001".into(),
        turn: 1,
        turn_kind: triage_cli::models::TurnKind::Codex,
        ts: chrono::Utc::now(),
        author: None,
        body: "prior".into(),
        evidence: vec![],
        provider: Some("codex".into()),
        model: Some("gpt-5.5".into()),
        tokens_in: None,
        tokens_out: None,
        elapsed_s: None,
        session_id: Some("01HPRIOR".into()),
        resumed: Some(false),
        action: None,
        outcome: None,
        drove_revision_from_turns: None,
        diff: None,
    };
    chat::append_turn(&chat::conversation_jsonl_path(&ticket_dir), &prior).unwrap();

    struct ResumeProvider;
    impl triage_cli::providers::LlmProvider for ResumeProvider {
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
                            triage_cli::providers::CompletionResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async { unreachable!("followup override is used") })
        }

        fn followup<'a>(
            &'a self,
            _session_id: Option<&'a str>,
            _prompt: &'a str,
            _system_prompt: &'a str,
            _model: &'a str,
            _attachments: &'a [triage_cli::models::Attachment],
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            triage_cli::providers::FollowupResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async {
                Ok(triage_cli::providers::FollowupResult {
                    text: "ok".into(),
                    tokens_in: None,
                    tokens_out: None,
                    session_id: Some("01HNEW".into()),
                    resumed: true,
                })
            })
        }
    }

    let reporter = RecordingChatReporter::default();
    followup_turn(
        &ticket_dir,
        "45001",
        "what next?",
        "",
        "gpt-5.5",
        &[],
        &ResumeProvider,
        Some(&reporter),
    )
    .await
    .unwrap();

    assert_eq!(
        reporter.stages.lock().unwrap().as_slice(),
        &[
            triage_cli::chat::ChatStage::ContextAssembled,
            triage_cli::chat::ChatStage::SessionResumeAttempt,
            triage_cli::chat::ChatStage::ProviderAwait,
            triage_cli::chat::ChatStage::ResponseParsed,
            triage_cli::chat::ChatStage::Saved,
        ]
    );
}

#[tokio::test]
async fn followup_turn_skips_resume_phase_when_no_prior_session() {
    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("45002");
    std::fs::create_dir_all(triage_cli::chat::session_dir(&ticket_dir)).unwrap();

    struct FirstProvider;
    impl triage_cli::providers::LlmProvider for FirstProvider {
        fn name(&self) -> &'static str {
            "fake-first"
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
                            triage_cli::providers::CompletionResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async {
                Ok(triage_cli::providers::CompletionResult {
                    text: "ok".into(),
                    tokens_in: None,
                    tokens_out: None,
                })
            })
        }
    }

    let reporter = RecordingChatReporter::default();
    followup_turn(
        &ticket_dir,
        "45002",
        "first question",
        "",
        "gpt-5.5",
        &[],
        &FirstProvider,
        Some(&reporter),
    )
    .await
    .unwrap();

    assert_eq!(
        reporter.stages.lock().unwrap().as_slice(),
        &[
            triage_cli::chat::ChatStage::ContextAssembled,
            triage_cli::chat::ChatStage::ProviderAwait,
            triage_cli::chat::ChatStage::ResponseParsed,
            triage_cli::chat::ChatStage::Saved,
        ]
    );
}

#[tokio::test]
async fn followup_turn_skips_resume_phase_for_non_codex_provider_with_prior_session() {
    use triage_cli::chat;

    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("45003");
    std::fs::create_dir_all(chat::session_dir(&ticket_dir)).unwrap();

    let prior = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "45003".into(),
        turn: 1,
        turn_kind: triage_cli::models::TurnKind::Codex,
        ts: chrono::Utc::now(),
        author: None,
        body: "prior".into(),
        evidence: vec![],
        provider: Some("codex".into()),
        model: Some("gpt-5.5".into()),
        tokens_in: None,
        tokens_out: None,
        elapsed_s: None,
        session_id: Some("01HPRIOR".into()),
        resumed: Some(false),
        action: None,
        outcome: None,
        drove_revision_from_turns: None,
        diff: None,
    };
    chat::append_turn(&chat::conversation_jsonl_path(&ticket_dir), &prior).unwrap();

    struct UnleashLikeProvider;
    impl triage_cli::providers::LlmProvider for UnleashLikeProvider {
        fn name(&self) -> &'static str {
            "unleash"
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
                            triage_cli::providers::CompletionResult,
                            triage_cli::providers::ProviderError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async {
                Ok(triage_cli::providers::CompletionResult {
                    text: "ok".into(),
                    tokens_in: None,
                    tokens_out: None,
                })
            })
        }
    }

    let reporter = RecordingChatReporter::default();
    followup_turn(
        &ticket_dir,
        "45003",
        "what next?",
        "",
        "gpt-5.5",
        &[],
        &UnleashLikeProvider,
        Some(&reporter),
    )
    .await
    .unwrap();

    assert_eq!(
        reporter.stages.lock().unwrap().as_slice(),
        &[
            triage_cli::chat::ChatStage::ContextAssembled,
            triage_cli::chat::ChatStage::ProviderAwait,
            triage_cli::chat::ChatStage::ResponseParsed,
            triage_cli::chat::ChatStage::Saved,
        ]
    );

    let parsed = triage_cli::chat::parse_conversation_jsonl(
        &triage_cli::chat::conversation_jsonl_path(&ticket_dir),
    )
    .unwrap();
    assert_eq!(
        parsed.turns.len(),
        2,
        "non-codex followup must not insert a system turn"
    );
    assert!(
        !parsed
            .turns
            .iter()
            .any(|t| matches!(t.turn_kind, triage_cli::models::TurnKind::System)),
        "non-codex providers cannot lose a codex subprocess session"
    );
}

#[tokio::test]
async fn revise_uses_base_ticket_snapshot_and_preserves_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("44776");
    std::fs::create_dir_all(ticket_dir.join(".session")).unwrap();

    // Seed a base-ticket and base-evidence snapshot
    let ticket = triage_cli::models::Ticket {
        id: 44776,
        subject: "audio dropped".into(),
        description: "".into(),
        requester_org: None,
        requester_email: None,
        tags: vec![],
        created_at: chrono::Utc::now(),
        updated_at: None,
        comments: vec![],
    };
    triage_cli::chat::write_base_ticket(&ticket_dir, &ticket).unwrap();
    triage_cli::chat::write_base_evidence_manifest(
        &ticket_dir,
        &triage_cli::models::BaseEvidenceManifest {
            schema: "triage-cli/base-evidence".into(),
            schema_version: 1,
            ticket_id: "44776".into(),
            captured_at: chrono::Utc::now(),
            evidence: vec![],
        },
    )
    .unwrap();

    // Seed an analyst follow-up turn WITH evidence (a paste)
    let analyst = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44776".into(),
        turn: 1,
        turn_kind: triage_cli::models::TurnKind::Analyst,
        ts: chrono::Utc::now(),
        author: Some("enrique".into()),
        body: "new evidence: reboot at 14:32".into(),
        evidence: vec![triage_cli::models::EvidenceProvenance::Paste {
            label: "note".into(),
            body: "reboot evidence".into(),
            bytes: 16,
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
    let conv_path = triage_cli::chat::conversation_jsonl_path(&ticket_dir);
    triage_cli::chat::append_turn(&conv_path, &analyst).unwrap();
    let analyst_pre = triage_cli::chat::parse_conversation_jsonl(&conv_path).unwrap();
    assert_eq!(analyst_pre.turns.len(), 1);

    // Hold the memory env scope so investigate_one_structured doesn't
    // try to open the real SQLite DB.
    let memory_md = dir.path().join("MEMORY.md");
    let memory_db = dir.path().join("data/memory.db");
    let _env = triage_cli::memory::MemoryEnvScope::new_with_tickets_root(
        &memory_md,
        &memory_db,
        Some(dir.path()),
    );

    // Call revise with no_llm=true (stub pipeline; no LLM API needed)
    let no_llm_opts = InvestigateOptions {
        no_llm: true,
        memory_hits_override: Some(vec![]),
        ..InvestigateOptions::defaults()
    };
    let outcome = revise(&ticket_dir, "44776", None, None, &no_llm_opts).await;
    assert!(outcome.is_ok(), "revise failed: {:?}", outcome.err());

    // Conversation must be preserved + extended with a system revise turn
    let after = triage_cli::chat::parse_conversation_jsonl(&conv_path).unwrap();
    assert!(after.turns.len() >= 2);
    let last = after.turns.last().unwrap();
    assert!(matches!(
        last.turn_kind,
        triage_cli::models::TurnKind::System
    ));
    assert_eq!(last.action.as_deref(), Some("revise"));
}

#[tokio::test]
async fn end_to_end_revise_against_fixture() {
    // Set up a self-contained end-to-end state in a tempdir mirroring
    // what the spec § 7.2 example would produce. This is a unit-test
    // version of the fixture (the actual fixture is deferred to v2).

    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("44776");
    std::fs::create_dir_all(ticket_dir.join(".session")).unwrap();

    // Seed base-ticket.json and base-evidence-manifest.json
    let ticket = triage_cli::models::Ticket {
        id: 44776,
        subject: "audio dropped".into(),
        description: "initial".into(),
        requester_org: None,
        requester_email: None,
        tags: vec![],
        created_at: chrono::Utc::now(),
        updated_at: None,
        comments: vec![],
    };
    triage_cli::chat::write_base_ticket(&ticket_dir, &ticket).unwrap();
    triage_cli::chat::write_base_evidence_manifest(
        &ticket_dir,
        &triage_cli::models::BaseEvidenceManifest {
            schema: "triage-cli/base-evidence".into(),
            schema_version: 1,
            ticket_id: "44776".into(),
            captured_at: chrono::Utc::now(),
            evidence: vec![],
        },
    )
    .unwrap();

    // Seed an analyst follow-up turn WITH evidence
    let analyst = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44776".into(),
        turn: 1,
        turn_kind: triage_cli::models::TurnKind::Analyst,
        ts: chrono::Utc::now(),
        author: Some("enrique".into()),
        body: "new log shows reboot at 14:32".into(),
        evidence: vec![triage_cli::models::EvidenceProvenance::Paste {
            label: "customer-note".into(),
            body: "reboot at 14:32 PT".into(),
            bytes: 18,
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
    let conv_path = triage_cli::chat::conversation_jsonl_path(&ticket_dir);
    triage_cli::chat::append_turn(&conv_path, &analyst).unwrap();

    // Hold the memory env scope so investigate_one_structured doesn't
    // try to open the real SQLite DB.
    let memory_md = dir.path().join("MEMORY.md");
    let memory_db = dir.path().join("data/memory.db");
    let _env = triage_cli::memory::MemoryEnvScope::new_with_tickets_root(
        &memory_md,
        &memory_db,
        Some(dir.path()),
    );

    // Call /revise with no_llm=true (stub pipeline; no LLM API needed)
    let no_llm_opts = InvestigateOptions {
        no_llm: true,
        memory_hits_override: Some(vec![]),
        ..InvestigateOptions::defaults()
    };
    revise(&ticket_dir, "44776", None, None, &no_llm_opts)
        .await
        .expect("revise must succeed");

    // Conversation now has: analyst turn-001 + system revise turn-002
    let parsed = triage_cli::chat::parse_conversation_jsonl(&conv_path).unwrap();
    assert_eq!(parsed.turns.len(), 2);
    let last = parsed.turns.last().unwrap();
    assert!(matches!(
        last.turn_kind,
        triage_cli::models::TurnKind::System
    ));
    assert_eq!(last.action.as_deref(), Some("revise"));
    assert!(
        last.drove_revision_from_turns
            .as_ref()
            .map(|v| v.contains(&1))
            .unwrap_or(false),
        "drove_revision_from_turns must include turn 1"
    );

    // CONVERSATION.md should also exist and contain both turns
    let md_path = triage_cli::chat::conversation_md_path(&ticket_dir);
    assert!(md_path.exists(), "CONVERSATION.md was not written");
    let md = std::fs::read_to_string(&md_path).unwrap();
    assert!(
        md.contains("turn-001 analyst"),
        "CONVERSATION.md missing turn-001 analyst header"
    );
    assert!(
        md.contains("turn-002 system"),
        "CONVERSATION.md missing turn-002 system header"
    );
}

#[tokio::test]
async fn revise_respects_soft_lock_when_force_unset() {
    // /revise must NOT silently overwrite another analyst's STATE.md.
    // Pre-seed a STATE.md whose owner differs from the current process
    // owner, then call revise() with opts.force = false. The pipeline
    // must surface a SoftLockConflict error and leave the existing
    // STATE.md intact.
    let dir = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("44889");
    std::fs::create_dir_all(ticket_dir.join(".session")).unwrap();

    let ticket = triage_cli::models::Ticket {
        id: 44889,
        subject: "soft-lock guard".into(),
        description: "".into(),
        requester_org: None,
        requester_email: None,
        tags: vec![],
        created_at: chrono::Utc::now(),
        updated_at: None,
        comments: vec![],
    };
    triage_cli::chat::write_base_ticket(&ticket_dir, &ticket).unwrap();
    triage_cli::chat::write_base_evidence_manifest(
        &ticket_dir,
        &triage_cli::models::BaseEvidenceManifest {
            schema: "triage-cli/base-evidence".into(),
            schema_version: 1,
            ticket_id: "44889".into(),
            captured_at: chrono::Utc::now(),
            evidence: vec![],
        },
    )
    .unwrap();

    let analyst = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44889".into(),
        turn: 1,
        turn_kind: triage_cli::models::TurnKind::Analyst,
        ts: chrono::Utc::now(),
        author: Some("enrique".into()),
        body: "new evidence: log dump".into(),
        evidence: vec![triage_cli::models::EvidenceProvenance::Paste {
            label: "note".into(),
            body: "fresh evidence".into(),
            bytes: 14,
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
    let conv_path = triage_cli::chat::conversation_jsonl_path(&ticket_dir);
    triage_cli::chat::append_turn(&conv_path, &analyst).unwrap();

    // Pre-seed STATE.md with a foreign owner. Use a sentinel value that
    // will never collide with the test machine's $USER.
    let foreign_state = "---\n\
ticket_id: 44889\n\
fork: B\n\
confidence: low\n\
quoted_rubric_row: \"\"\n\
rubric_version: \"2026-04-30\"\n\
owner: \"foreign-owner-not-this-test@triage.test\"\n\
created_at: 2026-05-13T07:32:11Z\n\
updated_at: 2026-05-13T07:32:11Z\n\
status: open\n\
related:\n  zendesk: []\n  jira: []\n  master: null\n\
cluster: null\n\
validator_warnings: []\n---\n";
    std::fs::write(ticket_dir.join("STATE.md"), foreign_state).unwrap();

    let memory_md = dir.path().join("MEMORY.md");
    let memory_db = dir.path().join("data/memory.db");
    let _env = triage_cli::memory::MemoryEnvScope::new_with_tickets_root(
        &memory_md,
        &memory_db,
        Some(dir.path()),
    );

    let opts = InvestigateOptions {
        no_llm: true,
        force: false,
        memory_hits_override: Some(vec![]),
        ..InvestigateOptions::defaults()
    };
    let outcome = revise(&ticket_dir, "44889", None, None, &opts).await;

    assert!(
        matches!(
            outcome,
            Err(PipelineError::TicketFolder(
                TicketFolderError::SoftLockConflict { .. }
            ))
        ),
        "expected SoftLockConflict, got {outcome:?}"
    );

    // The pre-seeded STATE.md must remain untouched on conflict.
    let post = std::fs::read_to_string(ticket_dir.join("STATE.md")).unwrap();
    assert!(
        post.contains("foreign-owner-not-this-test@triage.test"),
        "STATE.md was overwritten on soft-lock conflict"
    );
}

#[tokio::test]
async fn revise_does_not_mutate_tickets_root_env() {
    // /revise must not leave TRIAGE_TICKETS_ROOT in a different state than
    // it found it — that mutation leaks into concurrent inbox/watch work
    // running in the same process. The fix routes the destination through
    // `InvestigateOptions::tickets_root` instead of process-global env.
    let dir = tempfile::tempdir().unwrap();
    let other_root = tempfile::tempdir().unwrap();
    let ticket_dir = dir.path().join("44890");
    std::fs::create_dir_all(ticket_dir.join(".session")).unwrap();

    let ticket = triage_cli::models::Ticket {
        id: 44890,
        subject: "tickets-root guard".into(),
        description: "".into(),
        requester_org: None,
        requester_email: None,
        tags: vec![],
        created_at: chrono::Utc::now(),
        updated_at: None,
        comments: vec![],
    };
    triage_cli::chat::write_base_ticket(&ticket_dir, &ticket).unwrap();
    triage_cli::chat::write_base_evidence_manifest(
        &ticket_dir,
        &triage_cli::models::BaseEvidenceManifest {
            schema: "triage-cli/base-evidence".into(),
            schema_version: 1,
            ticket_id: "44890".into(),
            captured_at: chrono::Utc::now(),
            evidence: vec![],
        },
    )
    .unwrap();

    let analyst = triage_cli::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: "44890".into(),
        turn: 1,
        turn_kind: triage_cli::models::TurnKind::Analyst,
        ts: chrono::Utc::now(),
        author: Some("enrique".into()),
        body: "fresh paste".into(),
        evidence: vec![triage_cli::models::EvidenceProvenance::Paste {
            label: "note".into(),
            body: "evidence".into(),
            bytes: 8,
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
    let conv_path = triage_cli::chat::conversation_jsonl_path(&ticket_dir);
    triage_cli::chat::append_turn(&conv_path, &analyst).unwrap();

    // Set TRIAGE_TICKETS_ROOT to a path that is NOT the ticket's parent.
    // The mutation bug overwrites this to ticket_dir.parent(); a working
    // refactor leaves the env untouched.
    let memory_md = dir.path().join("MEMORY.md");
    let memory_db = dir.path().join("data/memory.db");
    let _env = triage_cli::memory::MemoryEnvScope::new_with_tickets_root(
        &memory_md,
        &memory_db,
        Some(other_root.path()),
    );

    let env_before = std::env::var("TRIAGE_TICKETS_ROOT").unwrap();
    assert_eq!(
        env_before,
        other_root.path().to_string_lossy(),
        "test setup did not seed the env correctly"
    );

    let opts = InvestigateOptions {
        no_llm: true,
        force: true, // bypass any soft-lock from a prior test
        tickets_root: Some(ticket_dir.parent().unwrap().to_path_buf()),
        memory_hits_override: Some(vec![]),
        ..InvestigateOptions::defaults()
    };
    revise(&ticket_dir, "44890", None, None, &opts)
        .await
        .expect("revise must succeed");

    let env_after = std::env::var("TRIAGE_TICKETS_ROOT").unwrap();
    assert_eq!(
        env_after, env_before,
        "revise mutated TRIAGE_TICKETS_ROOT from {env_before:?} to {env_after:?}"
    );

    // Sanity: writes still landed where requested (ticket_dir.parent()), not
    // under the env-set other_root.
    assert!(
        ticket_dir.join("STATE.md").exists(),
        "STATE.md was not written at ticket_dir; opts.tickets_root not honored"
    );
}

#[test]
fn base_evidence_legacy_v1_manifest_parses_with_none_bodies() {
    // Old v1 manifests on disk have no `body` field per entry. They must
    // deserialize cleanly into the v2 BaseEvidenceEntry shape, with
    // `body == None` everywhere (serde flatten + default).
    let dir = tempfile::tempdir().unwrap();
    let session_dir = dir.path().join(".session");
    std::fs::create_dir_all(&session_dir).unwrap();
    let manifest_path = session_dir.join("base-evidence-manifest.json");
    // Hand-rolled v1 JSON: evidence entries are flat EvidenceItem
    // objects with no `body` field.
    let v1_json = r#"{
        "schema": "triage-cli/base-evidence",
        "schema_version": 1,
        "ticket_id": "12345",
        "captured_at": "2026-05-12T00:00:00Z",
        "evidence": [
            {
                "id": "E-001",
                "kind": "datadog_log_window",
                "label": "site log window",
                "source_path": "datadog:log_window"
            },
            {
                "id": "E-002",
                "kind": "local_file",
                "label": "apex.log",
                "source_path": "local:apex.log"
            }
        ]
    }"#;
    std::fs::write(&manifest_path, v1_json).unwrap();
    let bem =
        triage_cli::chat::read_base_evidence_manifest(dir.path()).expect("v1 manifest must parse");
    assert_eq!(bem.evidence.len(), 2);
    for entry in &bem.evidence {
        assert!(
            entry.body.is_none(),
            "v1 entry {} unexpectedly carries a body: {:?}",
            entry.item.id,
            entry.body
        );
    }
}
