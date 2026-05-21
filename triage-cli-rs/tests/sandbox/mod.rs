//! Live sandbox integration tests.
//!
//! These tests require a real .env file with Zendesk credentials and are
//! gated behind `SANDBOX_INTEGRATION=1`. Run with:
//!
//!     SANDBOX_INTEGRATION=1 cargo test --test sandbox -- --nocapture
//!
//! Each test:
//! 1. Loads .env from the repo root (or TRIAGE_HOME).
//! 2. Validates that required env vars are present (but never prints secrets).
//! 3. Exercises a runbook workflow against the real sandbox.
//! 4. Asserts the expected output artifacts exist.
//!
//! NEVER commit secrets. NEVER print env var values.

mod runbook_02_live_triage;
mod runbook_05_live_provider;

use std::path::PathBuf;

fn sandbox_enabled() -> bool {
    std::env::var("SANDBOX_INTEGRATION").as_deref() == Ok("1")
}

fn load_sandbox_env() -> PathBuf {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let env_file = repo_root.join(".env");
    if env_file.exists() {
        let _ = dotenvy::from_path(&env_file);
    }
    repo_root
}

fn require_zendesk_env() {
    for var in &["ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN"] {
        let val = std::env::var(var).unwrap_or_default();
        assert!(!val.is_empty(), "{} must be set for sandbox tests", var);
    }
}
