//! Runbook 08: Read-only assigned-queue certification flow
//! Verifies the pipeline completes with mocked Zendesk and local evidence.

use super::common::acquire_env_lock;

use triage_cli::fixture::{FixtureDatadogClient, FixtureLoader};
use triage_cli::investigation;
use triage_cli::pipeline::{InvestigateOptions, SilentReporter};
use triage_cli::playbook::Rubric;
use triage_cli::zendesk::ZendeskSource;

use super::zendesk_mock::ZendeskFixtureClient;

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn certify_assigned_queue_triage_completes() {
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

    let fixture_zd = ZendeskFixtureClient::from_fixture("audio-drop");

    let my_ids = fixture_zd
        .list_my_ticket_ids()
        .await
        .expect("list_my_ticket_ids must succeed");
    assert!(
        my_ids.contains(&55001),
        "assigned queue must include fixture ticket 55001"
    );

    let ticket = fixture_zd
        .get_ticket(55001)
        .await
        .expect("get_ticket must succeed");
    assert_eq!(ticket.id, 55001);

    let mut session = investigation::create_session(ticket.clone());

    let loader = FixtureLoader::new(triage_cli::fixture::resolve_named("audio-drop"))
        .expect("fixture must exist");
    let logs = loader.load_datadog_logs().expect("logs must parse");

    let fixture_dd = FixtureDatadogClient::new(logs);
    let rubric = Rubric::load().expect("rubric must parse");

    let opts = InvestigateOptions {
        no_llm: true,
        force: true,
        ..InvestigateOptions::defaults()
    };

    let _outcome = triage_cli::pipeline::investigate_one_structured(
        ticket,
        &mut session,
        Some(&fixture_zd as &dyn ZendeskSource),
        Some(&fixture_dd as &dyn triage_cli::datadog::DatadogSource),
        &rubric,
        &SilentReporter,
        &opts,
    )
    .await
    .expect("certification pipeline must succeed");

    let ticket_id = "55001";
    for name in &[
        "INTAKE.md",
        "EVIDENCE_PREFLIGHT.md",
        "FORK_PACKET.md",
        "DRAFTS.md",
        "STATE.md",
    ] {
        assert!(
            dir.path().join(ticket_id).join(name).exists(),
            "{name} missing from ticket folder"
        );
    }

    std::env::remove_var("TRIAGE_HOME");
    std::env::remove_var("TRIAGE_MEMORY_MD");
    std::env::remove_var("TRIAGE_MEMORY_DB");
    std::env::remove_var("TRIAGE_TICKETS_ROOT");
}
