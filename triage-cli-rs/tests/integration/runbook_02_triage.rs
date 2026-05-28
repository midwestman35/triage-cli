//! Runbook 02: Investigate or triage a Zendesk ticket
//! End-to-end tests using fixture data + ZendeskFixtureClient.

use super::common::acquire_env_lock;

use triage_cli::datadog::DatadogSource;
use triage_cli::fixture::{FixtureDatadogClient, FixtureLoader};
use triage_cli::investigation;
use triage_cli::pipeline::{InvestigateOptions, SilentReporter};
use triage_cli::playbook::Rubric;
use triage_cli::zendesk::ZendeskSource;

use super::zendesk_mock::ZendeskFixtureClient;

#[allow(clippy::await_holding_lock)]
async fn run_fixture_pipeline(
    fixture_name: &str,
    opts: InvestigateOptions,
) -> (
    triage_cli::pipeline::StructuredInvestigation,
    tempfile::TempDir,
) {
    let _lock = acquire_env_lock();
    let dir = tempfile::tempdir().expect("tempdir");

    std::env::set_var("TRIAGE_HOME", dir.path().to_str().unwrap());
    std::env::set_var(
        "TRIAGE_MEMORY_MD",
        dir.path().join("MEMORY.md").to_str().unwrap(),
    );
    std::env::set_var(
        "TRIAGE_MEMORY_DB",
        dir.path().join("data/memory.db").to_str().unwrap(),
    );
    std::env::set_var("TRIAGE_TICKETS_ROOT", dir.path().to_str().unwrap());

    let loader = FixtureLoader::new(triage_cli::fixture::resolve_named(fixture_name))
        .expect("fixture must exist");
    let ticket = loader.load_ticket().expect("ticket must parse");
    let logs = loader.load_datadog_logs().expect("logs must parse");

    let mut opts = opts;
    if opts.tickets_root.is_none() {
        opts.tickets_root = Some(dir.path().to_path_buf());
    }

    let mut session = investigation::create_session(ticket.clone());
    let fixture_dd = FixtureDatadogClient::new(logs);
    let fixture_zd = ZendeskFixtureClient::from_fixture(fixture_name);
    let rubric = Rubric::load().expect("embedded rubric must parse");

    let outcome = triage_cli::pipeline::investigate_one_structured(
        ticket,
        &mut session,
        Some(&fixture_zd as &dyn ZendeskSource),
        Some(&fixture_dd as &dyn DatadogSource),
        &rubric,
        &SilentReporter,
        &opts,
    )
    .await
    .expect("pipeline should succeed");

    std::env::remove_var("TRIAGE_HOME");
    std::env::remove_var("TRIAGE_MEMORY_MD");
    std::env::remove_var("TRIAGE_MEMORY_DB");
    std::env::remove_var("TRIAGE_TICKETS_ROOT");

    (outcome, dir)
}

#[tokio::test]
async fn triage_produces_five_markdown_files() {
    let (_outcome, dir) = run_fixture_pipeline(
        "audio-drop",
        InvestigateOptions {
            no_llm: true,
            force: true,
            ..InvestigateOptions::defaults()
        },
    )
    .await;

    let ticket_dir = dir.path().join("55001");
    assert!(ticket_dir.join("INTAKE.md").exists(), "INTAKE.md missing");
    assert!(
        ticket_dir.join("EVIDENCE_PREFLIGHT.md").exists(),
        "EVIDENCE_PREFLIGHT.md missing"
    );
    assert!(
        ticket_dir.join("FORK_PACKET.md").exists(),
        "FORK_PACKET.md missing"
    );
    assert!(ticket_dir.join("DRAFTS.md").exists(), "DRAFTS.md missing");
    assert!(ticket_dir.join("STATE.md").exists(), "STATE.md missing");
}

#[tokio::test]
async fn triage_fixture_writes_operator_visible_base_evidence_delta() {
    let (_outcome, dir) = run_fixture_pipeline(
        "audio-drop",
        InvestigateOptions {
            no_llm: true,
            force: true,
            anchor_override: Some(
                "2026-05-14T15:32:00Z"
                    .parse()
                    .expect("fixture anchor must parse"),
            ),
            allow_unscoped_fixture_logs: true,
            ..InvestigateOptions::defaults()
        },
    )
    .await;

    let ticket_dir = dir.path().join("55001");
    let manifest_path = triage_cli::chat::base_evidence_path(&ticket_dir);
    assert!(
        manifest_path.exists(),
        "operator evidence proof missing: expected base evidence manifest at {} so the analyst can inspect the exact ticket/log evidence captured by the fixture run",
        manifest_path.display()
    );

    let manifest = triage_cli::chat::read_base_evidence_manifest(&ticket_dir)
        .expect("base evidence manifest must parse");
    assert_eq!(
        manifest.schema, "triage-cli/base-evidence",
        "operator evidence manifest must carry the canonical schema; manifest={manifest:#?}"
    );
    assert_eq!(
        manifest.schema_version, 2,
        "operator evidence manifest must be schema v2 so evidence bodies are inspectable; manifest={manifest:#?}"
    );
    assert_eq!(
        manifest.evidence.len(),
        3,
        "audio-drop fixture should give the operator 2 Zendesk comment bodies plus 1 Datadog log-window body; manifest={manifest:#?}"
    );

    let public_comment = manifest
        .evidence
        .iter()
        .find(|entry| entry.item.id == "E-001" && entry.item.kind == "zendesk_comment")
        .expect("E-001 public Zendesk comment must be present in operator evidence manifest");
    assert!(
        public_comment
            .body
            .as_deref()
            .unwrap_or_default()
            .contains("all 8 console stations"),
        "before/trigger: audio-drop fixture comment says all consoles are affected; now/surface: E-001 body in base-evidence-manifest.json must preserve that operator-visible fact; entry={public_comment:#?}"
    );

    let internal_comment = manifest
        .evidence
        .iter()
        .find(|entry| entry.item.id == "E-002" && entry.item.kind == "zendesk_comment")
        .expect("E-002 internal Zendesk comment must be present in operator evidence manifest");
    assert!(
        internal_comment
            .body
            .as_deref()
            .unwrap_or_default()
            .contains("RTP packet loss detected"),
        "before/trigger: fixture internal note identifies RTP packet loss; now/surface: E-002 body must preserve the exact operator-visible evidence; entry={internal_comment:#?}"
    );

    let datadog_window = manifest
        .evidence
        .iter()
        .find(|entry| entry.item.id == "E-003" && entry.item.kind == "datadog_log_window")
        .expect("E-003 Datadog log window must be present in operator evidence manifest");
    let datadog_body = datadog_window
        .body
        .as_deref()
        .expect("Datadog evidence entry must carry rendered log lines for operator inspection");
    assert_eq!(
        datadog_body.lines().count(),
        8,
        "before/trigger: audio-drop fixture ships 8 Datadog lines; now/surface: E-003 body must preserve all 8 lines; body={datadog_body}"
    );
    assert!(
        datadog_body.contains("AudioPipeline codec mismatch"),
        "operator evidence delta missing: Datadog body should prove the codec-mismatch signal is visible in the manifest; body={datadog_body}"
    );
}

#[tokio::test]
async fn triage_no_llm_produces_stub_fork_d() {
    let (outcome, _dir) = run_fixture_pipeline(
        "audio-drop",
        InvestigateOptions {
            no_llm: true,
            force: true,
            ..InvestigateOptions::defaults()
        },
    )
    .await;

    assert_eq!(
        outcome.report.fork_packet.commitment.fork_letter,
        triage_cli::models::ForkLetter::D,
        "no-llm stub must produce fork D"
    );
    assert_eq!(
        outcome.report.fork_packet.commitment.confidence,
        triage_cli::models::Confidence::Low,
        "no-llm stub must produce Low confidence"
    );
}

#[tokio::test]
async fn triage_with_site_override() {
    let (_outcome, dir) = run_fixture_pipeline(
        "no-site-map",
        InvestigateOptions {
            no_llm: true,
            site_override: Some("us-co-jeffcom-apex".into()),
            force: true,
            ..InvestigateOptions::defaults()
        },
    )
    .await;

    let folder = dir.path().join("55002");
    assert!(
        folder.join("STATE.md").exists(),
        "STATE.md missing with site override"
    );
}

#[tokio::test]
async fn triage_vendor_fork_fixture() {
    let (_outcome, dir) = run_fixture_pipeline(
        "vendor-fork",
        InvestigateOptions {
            no_llm: true,
            force: true,
            ..InvestigateOptions::defaults()
        },
    )
    .await;

    let id = dir.path().join("55004").join("STATE.md");
    assert!(id.exists(), "vendor-fork ticket folder missing");
}

#[tokio::test]
async fn triage_missing_evidence_fixture() {
    let (_outcome, dir) = run_fixture_pipeline(
        "missing-evidence",
        InvestigateOptions {
            no_llm: true,
            force: true,
            ..InvestigateOptions::defaults()
        },
    )
    .await;

    let id = dir.path().join("55003").join("STATE.md");
    assert!(id.exists(), "missing-evidence ticket folder missing");
}
