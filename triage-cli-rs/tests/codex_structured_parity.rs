//! Codex structured parity scaffold (safe by default).
//!
//! This test target is intentionally ignored until the app-server client exposes
//! a stable structured-turn contract. It compares the existing exec
//! `complete()` structured behavior with an app-server `turn/start` using
//! `outputSchema`.
//!
//! Future run command:
//!   CODEX_AVAILABLE=1 cargo test --test codex_structured_parity -- --ignored --nocapture

use std::env;

use serde_json::{json, Value};
use triage_cli::providers::codex::CodexSubprocessProvider;
use triage_cli::providers::codex_app_server::{CodexAppServerClient, StdioAppServerTransport};
use triage_cli::providers::LlmProvider;

const CONTRACT_MODEL: &str = "gpt-5.5";
const SYSTEM_PROMPT: &str =
    "Return only JSON that matches the requested shape. Do not include prose.";
const USER_PROMPT: &str = r#"Return exactly {"ok":true} and no prose."#;

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

#[tokio::test]
#[ignore = "requires CODEX_AVAILABLE=1 and live Codex exec/app-server calls"]
async fn structured_triage_exec_vs_app_server_parity() {
    if !codex_available() {
        eprintln!("skipped: set CODEX_AVAILABLE=1 and ensure `codex` is on PATH");
        return;
    }

    // This is deliberately a tiny structured fixture. Once the schema builder
    // is public, replace it with the full StructuredTriageReport fixture gate.
    let exec = CodexSubprocessProvider;
    let exec_output = exec
        .complete(USER_PROMPT, SYSTEM_PROMPT, CONTRACT_MODEL)
        .await
        .expect("codex exec complete should return structured text");
    let exec_json = json_object_from_response(&exec_output.text);
    assert_eq!(exec_json.get("ok").and_then(Value::as_bool), Some(true));

    let transport = StdioAppServerTransport::spawn()
        .await
        .expect("spawn codex app-server");
    let mut client = CodexAppServerClient::new(transport);
    client.initialize().await.expect("initialize");
    let thread_id = client
        .thread_start_ephemeral(SYSTEM_PROMPT, CONTRACT_MODEL)
        .await
        .expect("thread/start should return a thread id");
    let app_server_output = client
        .turn_start_collect(
            &thread_id,
            USER_PROMPT,
            CONTRACT_MODEL,
            Some(ok_object_schema()),
        )
        .await
        .expect("app-server structured turn should return text");
    let app_server_json = json_object_from_response(&app_server_output.text);
    assert_eq!(
        app_server_json.get("ok").and_then(Value::as_bool),
        Some(true)
    );
}
