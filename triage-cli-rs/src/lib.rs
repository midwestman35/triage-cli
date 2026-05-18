//! Triage-CLI library: ports `triage_cli` Python package to Rust.
//!
//! Layout mirrors the Python package one-module-per-file, plus a CLI entry point.

pub mod build_map;
pub mod chat;
pub mod cli;
pub mod datadog;
pub mod extract;
pub mod fixture;
pub mod interactive;
pub mod investigation;
pub mod llm;
pub mod memory;
pub mod models;
pub mod paths;
pub mod pipeline;
pub mod playbook;
pub mod providers;
pub mod redact;
pub mod setup;
pub mod ticket_folder;
pub mod tui;
pub mod watcher;
pub mod zendesk;

use std::process::ExitCode;

/// Load `.env` from `triage_home()` first so a standalone install (binary not
/// run from a repo checkout) still sees credentials written by `setup`.  Falls
/// back to `dotenvy::dotenv()` (cwd walk) for repo/dev usage where the two
/// paths coincide or where a local `.env` is preferred.
pub fn load_dotenv() {
    let home_env = crate::paths::triage_home().join(".env");
    if dotenvy::from_path(&home_env).is_err() {
        let _ = dotenvy::dotenv();
    }
}

/// Binary entry point. Delegates to the clap-derive CLI in `cli::run`.
pub fn run() -> ExitCode {
    load_dotenv();
    cli::run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialize tests that mutate global env vars to prevent races.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn load_dotenv_reads_sentinel_from_triage_home() {
        let _g = ENV_LOCK.lock().unwrap();

        // Create a temp dir with a .env containing a sentinel variable.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(".env"),
            "TRIAGE_TEST_SENTINEL=hello_from_home\n",
        )
        .unwrap();

        // Snapshot env state we'll touch.
        let prev_home = std::env::var(crate::paths::TRIAGE_HOME_ENV).ok();
        let prev_sentinel = std::env::var("TRIAGE_TEST_SENTINEL").ok();

        // Point TRIAGE_HOME at our temp dir and clear any stale sentinel.
        std::env::set_var(crate::paths::TRIAGE_HOME_ENV, tmp.path());
        std::env::remove_var("TRIAGE_TEST_SENTINEL");

        load_dotenv();

        let loaded = std::env::var("TRIAGE_TEST_SENTINEL").ok();

        // Restore env.
        std::env::remove_var("TRIAGE_TEST_SENTINEL");
        match prev_home {
            Some(v) => std::env::set_var(crate::paths::TRIAGE_HOME_ENV, v),
            None => std::env::remove_var(crate::paths::TRIAGE_HOME_ENV),
        }
        if let Some(v) = prev_sentinel {
            std::env::set_var("TRIAGE_TEST_SENTINEL", v);
        }

        assert_eq!(
            loaded.as_deref(),
            Some("hello_from_home"),
            "sentinel var should have been loaded from TRIAGE_HOME/.env"
        );
    }
}
