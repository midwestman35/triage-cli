//! CLI subprocess smoke tests: invoke the built `triage-cli` binary with the
//! exact flags described in each runbook, verify exit codes and output.
//!
//! Requires a release build: `cargo build --release`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn triage_cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join("triage-cli")
}

fn env_with_home(home_dir: &Path) -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("TRIAGE_HOME".into(), home_dir.to_str().unwrap().into());
    env.insert(
        "TRIAGE_TICKETS_ROOT".into(),
        home_dir.join("Tickets").to_str().unwrap().into(),
    );
    env
}

#[test]
fn runbook_02_cli_demo_audio_drop() {
    let home = tempfile::tempdir().expect("tempdir");
    let env = env_with_home(home.path());

    let output = Command::new(triage_cli_bin())
        .args(["demo", "audio-drop"])
        .envs(&env)
        .output()
        .expect("triage-cli demo must spawn");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "demo must exit 0\nstderr: {stderr}"
    );
}

#[test]
fn runbook_01_cli_doctor_flags_missing_env() {
    let home = tempfile::tempdir().expect("tempdir");
    let data_dir = home.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let mut env = env_with_home(home.path());
    env.insert("ZENDESK_SUBDOMAIN".into(), String::new());
    env.insert("ZENDESK_EMAIL".into(), String::new());
    env.insert("ZENDESK_API_TOKEN".into(), String::new());

    let output = Command::new(triage_cli_bin())
        .args(["doctor"])
        .envs(&env)
        .output()
        .expect("triage-cli doctor must spawn");

    assert!(
        !output.status.success(),
        "doctor must exit non-zero when env vars are missing\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn runbook_06_cli_watch_help() {
    let output = Command::new(triage_cli_bin())
        .args(["watch", "--help"])
        .output()
        .expect("triage-cli watch --help must spawn");

    assert!(output.status.success(), "watch --help must exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--view"), "help must mention --view");
}

#[test]
fn runbook_07_cli_inbox_help() {
    let output = Command::new(triage_cli_bin())
        .args(["inbox", "--help"])
        .output()
        .expect("triage-cli inbox --help must spawn");

    assert!(output.status.success(), "inbox --help must exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--view"), "help must mention --view");
}

#[test]
fn runbook_02_cli_triage_fixture() {
    let home = tempfile::tempdir().expect("tempdir");
    let data_dir = home.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let inventory = repo_root.join("apex-cnc-inventory.md");
    if !inventory.exists() {
        eprintln!("skipping: apex-cnc-inventory.md not found");
        return;
    }
    std::fs::copy(&inventory, home.path().join("apex-cnc-inventory.md")).expect("copy inventory");

    let mut env = env_with_home(home.path());

    let build_output = Command::new(triage_cli_bin())
        .args(["build-map"])
        .envs(&env)
        .output()
        .expect("triage-cli build-map must spawn");
    assert!(build_output.status.success(), "build-map must succeed");

    let fixture_path = triage_cli::fixture::resolve_named("audio-drop");
    env.insert(
        "TRIAGE_FIXTURES_DIR".into(),
        fixture_path.parent().unwrap().to_str().unwrap().into(),
    );

    // Use --force to bypass soft-lock on fresh dirs
    let output = Command::new(triage_cli_bin())
        .args([
            "triage",
            "55001",
            "--fixture",
            fixture_path.to_str().unwrap(),
            "--no-llm",
            "--force",
        ])
        .envs(&env)
        .output()
        .expect("triage-cli triage must spawn");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "triage must exit 0\nstderr: {stderr}"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Fork") || stdout.contains("fork"),
        "stdout must contain fork letter info:\n{stdout}"
    );
}

#[test]
fn runbook_03_cli_build_map() {
    let home = tempfile::tempdir().expect("tempdir");
    let data_dir = home.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let inventory = repo_root.join("apex-cnc-inventory.md");
    if !inventory.exists() {
        eprintln!("skipping: apex-cnc-inventory.md not found");
        return;
    }
    std::fs::copy(&inventory, home.path().join("apex-cnc-inventory.md")).expect("copy inventory");

    let env = env_with_home(home.path());

    let output = Command::new(triage_cli_bin())
        .args(["build-map"])
        .envs(&env)
        .output()
        .expect("triage-cli build-map must spawn");

    assert!(
        output.status.success(),
        "build-map must exit 0\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("cnc-map.json") || stdout.contains("entries"),
        "build-map stdout should mention output:\n{stdout}"
    );
}
