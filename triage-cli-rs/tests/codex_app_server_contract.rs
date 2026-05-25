//! Codex app-server contract smoke test (Phase 0).
//!
//! Verifies that a live `codex app-server --listen stdio://` process accepts
//! `initialize` over newline-delimited JSON-RPC. Skipped in CI unless the gate
//! env var is set and `codex` is on PATH.
//!
//! Run with:
//!   CODEX_AVAILABLE=1 cargo test --test codex_app_server_contract -- --nocapture
//!
//! Requires a working Codex CLI install with app-server support (see
//! `codex app-server --help`). No network assertion beyond what `initialize`
//! needs locally.

use std::env;

use triage_cli::providers::codex_app_server::{CodexAppServerClient, StdioAppServerTransport};

fn codex_available() -> bool {
    env::var("CODEX_AVAILABLE").as_deref() == Ok("1") && which::which("codex").is_ok()
}

#[tokio::test]
async fn initialize_smoke() {
    if !codex_available() {
        eprintln!("skipped: set CODEX_AVAILABLE=1 and ensure `codex` is on PATH");
        return;
    }

    let transport = StdioAppServerTransport::spawn()
        .await
        .expect("spawn codex app-server");
    let mut client = CodexAppServerClient::new(transport);
    client
        .initialize()
        .await
        .expect("initialize should succeed against live codex app-server");
    assert!(client.is_initialized());
}
