//! Runbook 05: Switch the LLM provider or model
//! Tests provider selection logic and --no-llm stub behavior.

use std::sync::Mutex;

use super::common::acquire_env_lock;

use triage_cli::fixture::{FixtureDatadogClient, FixtureLoader};
use triage_cli::investigation;
use triage_cli::pipeline::{InvestigateOptions, SilentReporter};
use triage_cli::playbook::Rubric;
use triage_cli::zendesk::ZendeskSource;

use super::zendesk_mock::ZendeskFixtureClient;

static PROVIDER_LOCK: Mutex<()> = Mutex::new(());

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn no_llm_produces_deterministic_stub() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _lock = acquire_env_lock();

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

    let loader = FixtureLoader::new(triage_cli::fixture::resolve_named("audio-drop"))
        .expect("fixture must exist");
    let ticket = loader.load_ticket().expect("ticket must parse");
    let logs = loader.load_datadog_logs().expect("logs must parse");
    let mut session = investigation::create_session(ticket.clone());
    let fixture_dd = FixtureDatadogClient::new(logs);
    let fixture_zd = ZendeskFixtureClient::from_fixture("audio-drop");
    let rubric = Rubric::load().expect("rubric must parse");

    let opts = InvestigateOptions {
        no_llm: true,
        force: true,
        ..InvestigateOptions::defaults()
    };

    let outcome = triage_cli::pipeline::investigate_one_structured(
        ticket,
        &mut session,
        Some(&fixture_zd as &dyn ZendeskSource),
        Some(&fixture_dd as &dyn triage_cli::datadog::DatadogSource),
        &rubric,
        &SilentReporter,
        &opts,
    )
    .await
    .expect("pipeline must succeed");

    assert_eq!(
        outcome.report.fork_packet.commitment.fork_letter.as_str(),
        "D",
    );
    assert_eq!(
        outcome.report.fork_packet.commitment.confidence.as_str(),
        "low",
    );
    assert!(
        outcome
            .report
            .fork_packet
            .commitment
            .reasoning
            .contains("Stub"),
        "stub reasoning must mention 'Stub': {:?}",
        outcome.report.fork_packet.commitment.reasoning,
    );

    std::env::remove_var("TRIAGE_HOME");
    std::env::remove_var("TRIAGE_MEMORY_MD");
    std::env::remove_var("TRIAGE_MEMORY_DB");
    std::env::remove_var("TRIAGE_TICKETS_ROOT");
}

#[test]
fn provider_selection_rejects_unknown() {
    let _lock = PROVIDER_LOCK.lock().unwrap();
    let prev = std::env::var("LLM_PROVIDER").ok();
    std::env::set_var("LLM_PROVIDER", "openai");
    let result = triage_cli::providers::get_provider();
    std::env::remove_var("LLM_PROVIDER");
    if let Some(v) = prev {
        std::env::set_var("LLM_PROVIDER", v);
    }
    assert!(result.is_err(), "unknown provider must be rejected");
    let err = result.err().expect("expected error");
    match err {
        triage_cli::providers::ProviderError::Unknown(name) => {
            assert_eq!(name, "openai");
        }
        other => panic!("expected Unknown error, got: {other}"),
    }
}
