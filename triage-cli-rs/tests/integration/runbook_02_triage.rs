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
