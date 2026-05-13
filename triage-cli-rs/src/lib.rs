//! Triage-CLI library: ports `triage_cli` Python package to Rust.
//!
//! Layout mirrors the Python package one-module-per-file, plus a CLI entry point.

pub mod build_map;
pub mod cli;
pub mod datadog;
pub mod extract;
pub mod interactive;
pub mod investigation;
pub mod llm;
pub mod memory;
pub mod models;
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

/// Binary entry point. Delegates to the clap-derive CLI in `cli::run`.
pub fn run() -> ExitCode {
    // Best-effort load of .env so every subcommand sees the same environment.
    let _ = dotenvy::dotenv();
    cli::run()
}
