//! Runbook 01: First-time setup
//! Tests doctor validation and build-map.

use std::fs;
use std::path::PathBuf;

use super::common::acquire_env_lock;

#[test]
fn build_map_produces_cnc_map_json() {
    let _lock = acquire_env_lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().to_path_buf();
    let data_dir = home.join("data");
    fs::create_dir_all(&data_dir).expect("create data dir");

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let inventory = repo_root.join("apex-cnc-inventory.md");
    if !inventory.exists() {
        eprintln!(
            "skipping: apex-cnc-inventory.md not found at {:?}",
            inventory
        );
        return;
    }
    fs::copy(&inventory, home.join("apex-cnc-inventory.md")).expect("copy inventory");

    let prev = std::env::var("TRIAGE_HOME").ok();
    std::env::set_var("TRIAGE_HOME", home.to_str().unwrap());
    let result = triage_cli::build_map::run();

    use std::process::ExitCode;
    assert!(
        matches!(result, ExitCode::SUCCESS),
        "build-map must succeed when inventory is present"
    );
    assert!(
        data_dir.join("cnc-map.json").exists(),
        "data/cnc-map.json must be produced by build-map"
    );

    match &prev {
        Some(v) => std::env::set_var("TRIAGE_HOME", v),
        None => std::env::remove_var("TRIAGE_HOME"),
    }
}
