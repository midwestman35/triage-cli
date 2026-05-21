//! Runbook 05 (live): Verify doctor passes with valid env.
//!
//! Uses a subprocess invocation of `triage-cli doctor` since the library
//! function returns `ExitCode` rather than a structured result.

use std::path::PathBuf;
use std::process::Command;

use crate::{load_sandbox_env, sandbox_enabled};

fn triage_cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join("triage-cli")
}

#[test]
fn live_doctor_passes_with_valid_env() {
    if !sandbox_enabled() {
        eprintln!("skipped: set SANDBOX_INTEGRATION=1 to run live sandbox tests");
        return;
    }
    load_sandbox_env();

    let output = Command::new(triage_cli_bin())
        .args(["doctor"])
        .output()
        .expect("triage-cli doctor must spawn");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "doctor must exit 0 when env vars are correctly configured\nstderr: {stderr}"
    );
}
