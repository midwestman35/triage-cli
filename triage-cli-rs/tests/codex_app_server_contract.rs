//! Codex app-server contract smoke tests (gated on `CODEX_AVAILABLE=1`).
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

use serde_json::{json, Value};
use triage_cli::providers::codex_app_server::{CodexAppServerClient, StdioAppServerTransport};

const CONTRACT_MODEL: &str = "gpt-5.5";
const FUTURE_INTERRUPT_API_TODO: &str = "TODO: wire to the public active-turn API once it exists, e.g. a client/runtime method that exposes turn_id before drain completion, accepts an interrupt command while the turn is in flight, and observes status=interrupted.";

fn codex_available() -> bool {
    env::var("CODEX_AVAILABLE").as_deref() == Ok("1") && which::which("codex").is_ok()
}

fn ok_object_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ok"],
        "properties": {
            "ok": { "type": "boolean" }
        }
    })
}

fn json_object_from_response(text: &str) -> Value {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return value;
    }
    let Some(start) = trimmed.find('{') else {
        panic!("structured response did not contain a JSON object: {trimmed}");
    };
    let Some(end) = trimmed.rfind('}') else {
        panic!("structured response did not contain a complete JSON object: {trimmed}");
    };
    serde_json::from_str(&trimmed[start..=end])
        .unwrap_or_else(|e| panic!("structured response JSON did not parse: {e}\n{trimmed}"))
}

enum InterruptSmoke {
    MissingActiveTurnApi(&'static str),
}

async fn interrupt_smoke_via_public_api() -> InterruptSmoke {
    InterruptSmoke::MissingActiveTurnApi(FUTURE_INTERRUPT_API_TODO)
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

#[tokio::test]
async fn thread_start_smoke() {
    if !codex_available() {
        eprintln!("skipped: set CODEX_AVAILABLE=1 and ensure `codex` is on PATH");
        return;
    }

    let transport = StdioAppServerTransport::spawn()
        .await
        .expect("spawn codex app-server");
    let mut client = CodexAppServerClient::new(transport);
    client.initialize().await.expect("initialize");
    let thread_id = client
        .thread_start("You are a concise assistant.", CONTRACT_MODEL)
        .await
        .expect("thread/start should return a thread id");
    assert!(!thread_id.is_empty(), "thread id must be non-empty");
}

#[tokio::test]
async fn structured_turn_output_schema_smoke() {
    if !codex_available() {
        eprintln!("skipped: set CODEX_AVAILABLE=1 and ensure `codex` is on PATH");
        return;
    }

    let transport = StdioAppServerTransport::spawn()
        .await
        .expect("spawn codex app-server");
    let mut client = CodexAppServerClient::new(transport);
    client.initialize().await.expect("initialize");
    let thread_id = client
        .thread_start_ephemeral(
            "Return only JSON that matches the provided output schema.",
            CONTRACT_MODEL,
        )
        .await
        .expect("thread/start should return a thread id");
    let output = client
        .turn_start_collect(
            &thread_id,
            r#"Return exactly {"ok":true} and no prose."#,
            CONTRACT_MODEL,
            Some(ok_object_schema()),
        )
        .await
        .expect("structured turn/start should complete");

    assert_eq!(output.thread_id, thread_id);
    assert!(!output.turn_id.is_empty(), "turn id must be non-empty");
    assert_eq!(
        output.status.as_deref(),
        Some("completed"),
        "structured turn should complete successfully"
    );
    let parsed = json_object_from_response(&output.text);
    assert_eq!(parsed.get("ok").and_then(Value::as_bool), Some(true));
}

#[tokio::test]
#[ignore = "requires CODEX_AVAILABLE=1 and the future public active-turn interrupt API"]
async fn interrupt_long_turn_smoke_placeholder() {
    if !codex_available() {
        eprintln!("skipped: set CODEX_AVAILABLE=1 and ensure `codex` is on PATH");
        return;
    }

    // Desired live contract:
    // 1. initialize + thread/start with app-server.
    // 2. start a deliberately long-running turn through public client API that
    //    exposes the active turn_id without blocking an interrupt command.
    // 3. issue turn/interrupt within about two seconds.
    // 4. wait for matching turn/completed with status="interrupted".
    match interrupt_smoke_via_public_api().await {
        InterruptSmoke::MissingActiveTurnApi(todo) => {
            eprintln!("skipped: {todo}");
        }
    }
}
