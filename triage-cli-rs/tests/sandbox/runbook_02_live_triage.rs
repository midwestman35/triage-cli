//! Runbook 02 (live): Triage a real ticket from the assigned queue.
//!
//! Fetches a ticket from the authenticated user's Zendesk queue, runs
//! `investigate_one_structured` with `no_llm: true`, and verifies all
//! five markdown files are produced.

use triage_cli::investigation;
use triage_cli::pipeline::{InvestigateOptions, SilentReporter};
use triage_cli::playbook::Rubric;
use triage_cli::zendesk::{ZendeskClient, ZendeskSource};

use crate::{load_sandbox_env, require_zendesk_env, sandbox_enabled};

#[tokio::test]
async fn live_triage_assigned_queue_ticket() {
    if !sandbox_enabled() {
        eprintln!("skipped: set SANDBOX_INTEGRATION=1 to run live sandbox tests");
        return;
    }
    load_sandbox_env();
    require_zendesk_env();

    let dir = tempfile::tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    // Isolate pipeline outputs to the temp directory
    let home = dir.path().to_path_buf();
    let prev_home = std::env::var("TRIAGE_HOME").ok();
    let prev_md = std::env::var("TRIAGE_MEMORY_MD").ok();
    let prev_db = std::env::var("TRIAGE_MEMORY_DB").ok();
    let prev_root = std::env::var("TRIAGE_TICKETS_ROOT").ok();
    std::env::set_var("TRIAGE_HOME", home.to_str().unwrap());
    std::env::set_var("TRIAGE_MEMORY_MD", home.join("MEMORY.md").to_str().unwrap());
    std::env::set_var(
        "TRIAGE_MEMORY_DB",
        home.join("data/memory.db").to_str().unwrap(),
    );
    std::env::set_var("TRIAGE_TICKETS_ROOT", home.to_str().unwrap());

    let zd =
        ZendeskClient::from_env().expect("ZendeskClient::from_env must succeed with valid env");
    let my_ids = zd
        .list_my_ticket_ids()
        .await
        .expect("list_my_ticket_ids must succeed");
    if my_ids.is_empty() {
        // Restore env vars before early return
        restore_env("TRIAGE_HOME", prev_home);
        restore_env("TRIAGE_MEMORY_MD", prev_md);
        restore_env("TRIAGE_MEMORY_DB", prev_db);
        restore_env("TRIAGE_TICKETS_ROOT", prev_root);
        eprintln!("skipped: assigned queue is empty");
        return;
    }

    let ticket_id = my_ids[0];
    let ticket = zd
        .get_ticket(ticket_id)
        .await
        .expect("get_ticket must succeed");
    let mut session = investigation::create_session(ticket.clone());

    let rubric = Rubric::load().expect("rubric must parse");
    let opts = InvestigateOptions {
        no_llm: true,
        force: true,
        tickets_root: Some(home.clone()),
        ..InvestigateOptions::defaults()
    };

    let _outcome = triage_cli::pipeline::investigate_one_structured(
        ticket,
        &mut session,
        Some(&zd as &dyn ZendeskSource),
        None,
        &rubric,
        &SilentReporter,
        &opts,
    )
    .await
    .expect("live triage must succeed");

    let id_str = ticket_id.to_string();
    for name in &[
        "INTAKE.md",
        "EVIDENCE_PREFLIGHT.md",
        "FORK_PACKET.md",
        "DRAFTS.md",
        "STATE.md",
    ] {
        assert!(
            home.join(&id_str).join(name).exists(),
            "{name} missing from live ticket folder"
        );
    }

    // Restore env vars
    restore_env("TRIAGE_HOME", prev_home);
    restore_env("TRIAGE_MEMORY_MD", prev_md);
    restore_env("TRIAGE_MEMORY_DB", prev_db);
    restore_env("TRIAGE_TICKETS_ROOT", prev_root);
}

fn restore_env(key: &str, prev: Option<String>) {
    match prev {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}
