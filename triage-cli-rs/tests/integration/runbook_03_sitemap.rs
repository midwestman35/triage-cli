//! Runbook 03: Refresh the CNC site map
//! Tests that build-map produces valid entries with required fields.

use std::fs;
use std::path::PathBuf;

use super::common::acquire_env_lock;

#[test]
fn build_map_produces_entries_with_required_fields() {
    let _lock = acquire_env_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();
    let data_dir = home.join("data");
    fs::create_dir_all(&data_dir).expect("create data dir");

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let inventory = repo_root.join("apex-cnc-inventory.md");
    if !inventory.exists() {
        eprintln!("skipping: apex-cnc-inventory.md not found");
        return;
    }
    fs::copy(&inventory, home.join("apex-cnc-inventory.md")).expect("copy inventory");

    let prev = std::env::var("TRIAGE_HOME").ok();
    std::env::set_var("TRIAGE_HOME", home.to_str().unwrap());
    let _ = triage_cli::build_map::run();

    let map_path = data_dir.join("cnc-map.json");
    assert!(map_path.exists(), "cnc-map.json must exist after build-map");

    let map_text = fs::read_to_string(&map_path).expect("read cnc-map.json");
    let entries: Vec<triage_cli::models::SiteEntry> =
        serde_json::from_str(&map_text).expect("cnc-map.json must be valid JSON");

    for entry in &entries {
        assert!(
            !entry.site_name.is_empty(),
            "site_name must not be empty: {:?}",
            entry
        );
        assert!(
            !entry.cnc.is_empty(),
            "cnc UUID must not be empty: {:?}",
            entry
        );
    }

    assert!(
        entries.len() >= 30,
        "cnc-map.json must have at least 30 entries, got {}",
        entries.len()
    );

    match &prev {
        Some(v) => std::env::set_var("TRIAGE_HOME", v),
        None => std::env::remove_var("TRIAGE_HOME"),
    }
}
