//! Runbook 06: Watch a Zendesk view
//! Tests watcher state logic: should_triage, state round-trips, prune_state.

use chrono::Utc;
use triage_cli::models::Ticket;
use triage_cli::watcher::{load_state, save_state, should_triage, State};

fn make_ticket(id: u64, updated: &str) -> Ticket {
    use chrono::TimeZone;
    let created = Utc.with_ymd_and_hms(2026, 5, 14, 12, 0, 0).unwrap();
    let updated_at = chrono::DateTime::parse_from_rfc3339(updated)
        .expect("valid timestamp")
        .with_timezone(&chrono::Utc);
    Ticket {
        id,
        subject: format!("Test ticket {id}"),
        description: "test description".into(),
        requester_org: Some("TestOrg".into()),
        requester_email: Some("test@example.com".into()),
        tags: vec![],
        created_at: created,
        updated_at: Some(updated_at),
        comments: vec![],
    }
}

#[test]
fn should_triage_new_ticket_within_backfill() {
    let state = State {
        version: 1,
        triaged: std::collections::BTreeMap::new(),
    };
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    let updated_time = cutoff + chrono::Duration::seconds(1);
    let ticket = make_ticket(
        12345,
        &updated_time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    assert!(should_triage(&ticket, &state, cutoff));
}

#[test]
fn should_triage_skips_old_ticket_outside_backfill() {
    let state = State {
        version: 1,
        triaged: std::collections::BTreeMap::new(),
    };
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    let old_time = Utc::now() - chrono::Duration::hours(48);
    let ticket = make_ticket(
        12345,
        &old_time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    assert!(!should_triage(&ticket, &state, cutoff));
}

#[test]
fn should_triage_retriages_on_updated_at_advance() {
    let mut state = State {
        version: 1,
        triaged: std::collections::BTreeMap::new(),
    };
    let old_ts = Utc::now() - chrono::Duration::hours(2);
    state.triaged.insert(
        "12345".into(),
        old_ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    let new_ts = Utc::now() - chrono::Duration::minutes(30);
    let ticket = make_ticket(
        12345,
        &new_ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );
    assert!(should_triage(&ticket, &state, cutoff));
}

#[test]
fn should_triage_skips_unchanged() {
    let mut state = State {
        version: 1,
        triaged: std::collections::BTreeMap::new(),
    };
    let ts = Utc::now() - chrono::Duration::minutes(30);
    let ts_str = ts.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    state.triaged.insert("12345".into(), ts_str.clone());
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    let ticket = make_ticket(12345, &ts_str);
    assert!(!should_triage(&ticket, &state, cutoff));
}

#[test]
fn state_round_trips_to_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("watcher-state-test.json");
    let mut state = State {
        version: 1,
        triaged: std::collections::BTreeMap::new(),
    };
    state
        .triaged
        .insert("99887".into(), "2026-05-07T14:32:04+00:00".into());
    state
        .triaged
        .insert("99888".into(), "2026-05-08T09:15:00+00:00".into());

    save_state(&path, &state).expect("save must succeed");
    let loaded = load_state(&path).expect("load must succeed");
    assert_eq!(loaded.triaged.len(), 2);
    assert_eq!(
        loaded.triaged.get("99887").unwrap(),
        "2026-05-07T14:32:04+00:00"
    );
}

#[test]
fn prune_state_caps_at_max_entries() {
    let mut state = State {
        version: 1,
        triaged: std::collections::BTreeMap::new(),
    };
    for i in 0..20u64 {
        state.triaged.insert(
            format!("{i}"),
            format!("2026-05-{:02}T00:00:00+00:00", (i % 28) + 1),
        );
    }
    let pruned =
        triage_cli::watcher::prune_state(state, 10, 365, &std::collections::HashSet::new());
    assert!(pruned.triaged.len() <= 10);
}
