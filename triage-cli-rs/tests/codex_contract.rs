//! Codex contract gate (spec § 5.6). Empirically determines how to extract
//! a session ID from the `codex` CLI so the `followup` provider impl can
//! resume sessions cheaply. Skipped in CI unless `CODEX_AVAILABLE=1` is set.
//!
//! Run with:
//!   CODEX_AVAILABLE=1 cargo test --test codex_contract -- --nocapture
//!
//! See `docs/decisions/2026-05-17-codex-session-capture.md` for the
//! capture-method decision driven by these tests.
//!
//! ## Local-run timing
//!
//! Tests run codex subprocesses synchronously (`Command::output()` with no
//! timeout). A typical run is ~50 seconds wall-clock for all four tests. A
//! hung codex (network stall, expired auth, etc.) will block a local cargo
//! test run indefinitely — CI is safe because tests skip without
//! `CODEX_AVAILABLE=1`. A timeout helper is tracked as a v2 improvement.

use std::env;
use std::process::Command;

fn codex_available() -> bool {
    env::var("CODEX_AVAILABLE").as_deref() == Ok("1") && which::which("codex").is_ok()
}

/// Model passed to `codex exec` in the contract tests. `gpt-5.5` is the
/// org-default model that's known to be available to the configured
/// `chatgpt` auth_mode (see `~/.codex/config.toml`).
const CONTRACT_MODEL: &str = "gpt-5.5";

#[test]
fn capture_method_json() {
    if !codex_available() {
        eprintln!("skipped: set CODEX_AVAILABLE=1 and ensure `codex` is on PATH");
        return;
    }
    let out = Command::new("codex")
        .args([
            "exec",
            "--skip-git-repo-check",
            "--json",
            "--model",
            CONTRACT_MODEL,
            "reply with exactly the word 'hi'",
        ])
        .output()
        .expect("codex exec --json failed to spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let has_thread_id = stdout.contains("\"thread_id\"");
    let has_session_id = stdout.contains("\"session_id\"");
    eprintln!("exit={:?}", out.status.code());
    eprintln!("--json carries thread_id: {has_thread_id}");
    eprintln!("--json carries session_id: {has_session_id}");
    eprintln!("--- stdout ---\n{stdout}");
    eprintln!("--- stderr ---\n{stderr}");
    assert!(
        has_thread_id,
        "codex exec --json no longer emits thread_id — decision doc needs revisiting"
    );
}

#[test]
fn capture_method_stderr_regex() {
    // Informational only — no assertion; documents Method B (stderr regex) as a
    // fallback shape, not the selected capture method (see decision doc).
    if !codex_available() {
        eprintln!("skipped: set CODEX_AVAILABLE=1 and ensure `codex` is on PATH");
        return;
    }
    let out = Command::new("codex")
        .args([
            "exec",
            "--skip-git-repo-check",
            "--model",
            CONTRACT_MODEL,
            "reply with exactly the word 'hi'",
        ])
        .output()
        .expect("codex exec failed to spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let session_line = stderr.lines().find(|l| {
        let lower = l.to_ascii_lowercase();
        lower.contains("session_id=")
            || lower.contains("session id:")
            || lower.contains("session: ")
            || lower.contains("thread_id=")
            || lower.contains("thread id:")
    });
    eprintln!("exit={:?}", out.status.code());
    eprintln!("stderr session line found: {session_line:?}");
    eprintln!("--- full stderr ---\n{stderr}");
}

#[test]
fn resume_round_trip() {
    if !codex_available() {
        eprintln!("skipped: set CODEX_AVAILABLE=1 and ensure `codex` is on PATH");
        return;
    }

    // Capture method A: parse `codex exec --json` stdout as JSON-Lines and
    // pluck `thread_id` out of the first `thread.started` event. See
    // `docs/decisions/2026-05-17-codex-session-capture.md`.
    let first = Command::new("codex")
        .args([
            "exec",
            "--skip-git-repo-check",
            "--json",
            "--model",
            CONTRACT_MODEL,
            "Remember the number 4242. Reply with only the word 'ok'.",
        ])
        .output()
        .expect("first codex exec failed");
    assert!(
        first.status.success(),
        "first codex exec exited non-zero: {:?}\nstderr: {}",
        first.status.code(),
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    // The capture rule: first JSONL record that exposes a top-level
    // `thread_id` (codex 0.130 emits this on a `thread.started` event as
    // the very first line). We don't insist on `type=="thread.started"`
    // because future codex versions could rename the event while keeping
    // the field stable — the round-trip assertion below catches drift
    // either way.
    let thread_id = first_stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|v| v.get("thread_id").and_then(|t| t.as_str()).map(String::from))
        .expect("thread_id not found in first --json stdout");

    eprintln!("captured thread_id: {thread_id}");

    let second = Command::new("codex")
        .args([
            "exec",
            "resume",
            &thread_id,
            "--skip-git-repo-check",
            "--json",
            "--model",
            CONTRACT_MODEL,
            "What number did I ask you to remember? Reply with only the number.",
        ])
        .output()
        .expect("codex exec resume failed");
    assert!(
        second.status.success(),
        "resume exited non-zero: {:?}\nstderr: {}",
        second.status.code(),
        String::from_utf8_lossy(&second.stderr)
    );
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    eprintln!("--- resume stdout ---\n{second_stdout}");

    // Look for the answer inside any `agent_message` item.
    let answered_with_4242 = second_stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .any(|v| {
            v.get("item")
                .and_then(|item| item.get("text"))
                .and_then(|t| t.as_str())
                .map(|s| s.contains("4242"))
                .unwrap_or(false)
        });

    assert!(
        answered_with_4242,
        "resumed session did not recall the number — capture method may be wrong\nstdout: {second_stdout}"
    );
}

#[test]
fn session_expired_surface() {
    if !codex_available() {
        eprintln!("skipped: set CODEX_AVAILABLE=1 and ensure `codex` is on PATH");
        return;
    }
    // Document the surface codex exposes when asked to resume a UUID that
    // does not correspond to any stored rollout. The `providers/codex.rs`
    // followup impl uses this to decide whether to retry with a fresh
    // session (replay-context) or surface the error to the user.
    let out = Command::new("codex")
        .args([
            "exec",
            "resume",
            "00000000-0000-0000-0000-000000000000",
            "--skip-git-repo-check",
            "--model",
            CONTRACT_MODEL,
            "hello",
        ])
        .output()
        .expect("codex exec resume failed to spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!("exit={:?}", out.status.code());
    eprintln!("--- stdout ---\n{stdout}");
    eprintln!("--- stderr ---\n{stderr}");

    assert!(
        !out.status.success(),
        "codex resume with bogus UUID unexpectedly succeeded"
    );
    assert!(
        stderr.contains("no rollout found for thread id"),
        "session-expired stderr surface changed — update decision doc and providers/codex.rs fallback path"
    );
}
