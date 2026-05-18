//! Codex provider — subprocess to `codex exec`.
//!
//! No Python equivalent (see `REGRESSIONS.md` R2). The system prompt is
//! prepended to the user prompt with a `## System` / `## User` header pair
//! because `codex exec` accepts a single prompt argument; this keeps the
//! intent preserved while still using a single subprocess call.

use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;

use tokio::process::Command;

use super::{CompletionResult, FollowupResult, LlmProvider, ProviderError};

/// Default model passed via `codex exec --model` when `CODEX_MODEL` is unset.
/// Single source of truth — `llm::model_for_provider` references this.
pub const DEFAULT_CODEX_MODEL: &str = "gpt-5.5";

pub struct CodexSubprocessProvider;

impl LlmProvider for CodexSubprocessProvider {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn complete<'a>(
        &'a self,
        prompt: &'a str,
        system_prompt: &'a str,
        model: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionResult, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            if which::which("codex").is_err() {
                return Err(ProviderError::SubprocessMissing("codex"));
            }
            let combined = if system_prompt.is_empty() {
                prompt.to_string()
            } else {
                format!("## System\n{system_prompt}\n\n## User\n{prompt}")
            };
            let output = Command::new("codex")
                .arg("exec")
                .arg("--model")
                .arg(model)
                .arg(&combined)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|e| ProviderError::SubprocessFailure("codex", e.to_string()))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                return Err(ProviderError::SubprocessFailure(
                    "codex",
                    format!("exit {:?}: {}", output.status.code(), stderr.trim()),
                ));
            }
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            // Codex subprocess does not expose token counts.
            Ok(CompletionResult {
                text,
                tokens_in: None,
                tokens_out: None,
            })
        })
    }

    fn followup<'a>(
        &'a self,
        session_id: Option<&'a str>,
        prompt: &'a str,
        system_prompt: &'a str,
        model: &'a str,
        attachments: &'a [crate::models::Attachment],
    ) -> Pin<Box<dyn Future<Output = Result<FollowupResult, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            if which::which("codex").is_err() {
                return Err(ProviderError::SubprocessMissing("codex"));
            }
            let stamped = super::stamp_attachments_into_prompt(prompt, attachments);
            let combined = if system_prompt.is_empty() {
                stamped
            } else {
                format!("## System\n{system_prompt}\n\n## User\n{stamped}")
            };

            // Try native resume first if we have a session ID.
            if let Some(sid) = session_id {
                let out = Command::new("codex")
                    .args([
                        "exec",
                        "resume",
                        sid,
                        "--skip-git-repo-check",
                        "--json",
                        "--model",
                        model,
                        &combined,
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .await
                    .map_err(|e| ProviderError::SubprocessFailure("codex", e.to_string()))?;

                if out.status.success() {
                    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
                    let text = extract_agent_message(&stdout).unwrap_or(stdout.clone());
                    // The resumed turn echoes the same thread_id back on thread.started;
                    // fall back to the original sid if for some reason it is absent.
                    let new_sid =
                        extract_session_id_from_json(&stdout).or_else(|| Some(sid.to_string()));
                    return Ok(FollowupResult {
                        text,
                        tokens_in: None,
                        tokens_out: None,
                        session_id: new_sid,
                        resumed: true,
                    });
                }

                // Non-success: check whether this is the known session-lost surface.
                // If it is NOT, propagate as a real error rather than silently replaying.
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !looks_like_session_lost(&stderr) {
                    return Err(ProviderError::SubprocessFailure(
                        "codex",
                        format!("exit {:?}: {}", out.status.code(), stderr.trim()),
                    ));
                }
                // Session-lost — fall through to the non-resume path below.
            }

            // Non-resume path: no session ID, or resume failed with session-lost.
            // Uses `--skip-git-repo-check` and `--json` for consistency with
            // the contract test (see `tests/codex_contract.rs`).
            let out = Command::new("codex")
                .args([
                    "exec",
                    "--skip-git-repo-check",
                    "--json",
                    "--model",
                    model,
                    &combined,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .map_err(|e| ProviderError::SubprocessFailure("codex", e.to_string()))?;

            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                return Err(ProviderError::SubprocessFailure(
                    "codex",
                    format!("exit {:?}: {}", out.status.code(), stderr.trim()),
                ));
            }
            let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            let text = extract_agent_message(&stdout).unwrap_or(stdout.clone());
            let new_sid = extract_session_id_from_json(&stdout);
            Ok(FollowupResult {
                text,
                tokens_in: None,
                tokens_out: None,
                session_id: new_sid,
                resumed: false,
            })
        })
    }
}

/// Extract the codex session ID (codex's "thread_id" field) from a
/// `codex exec --json` stdout stream. The first record is typically a
/// `{"type":"thread.started","thread_id":"<uuid>"}` JSONL line.
///
/// The wire-format field is `session_id` per the spec; codex's actual
/// field is `thread_id`. This translation is documented in
/// `docs/decisions/2026-05-17-codex-session-capture.md`.
fn extract_session_id_from_json(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(tid) = v.get("thread_id").and_then(|t| t.as_str()) {
                return Some(tid.to_string());
            }
        }
    }
    None
}

/// Extract the agent-message body text from a `codex exec --json` stream.
///
/// Handles two record shapes observed in codex 0.130.0:
/// - `{"type":"agent_message","item":{"text":"..."}}` — as emitted by mock
///   scripts and some codex versions.
/// - `{"type":"item.completed","item":{"type":"agent_message","text":"..."}}` —
///   as emitted by real codex 0.130.0 (see acceptance evidence in
///   `docs/decisions/2026-05-17-codex-session-capture.md`).
///
/// Returns `None` if no matching record is found; the caller falls back to
/// the raw stdout.
fn extract_agent_message(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            let ty = v.get("type").and_then(|t| t.as_str());
            // Shape 1: {"type":"agent_message","item":{"text":"..."}}
            if ty == Some("agent_message") {
                if let Some(text) = v.pointer("/item/text").and_then(|t| t.as_str()) {
                    return Some(text.to_string());
                }
            }
            // Shape 2: {"type":"item.completed","item":{"type":"agent_message","text":"..."}}
            if ty == Some("item.completed") {
                if let Some(item) = v.get("item") {
                    if item.get("type").and_then(|t| t.as_str()) == Some("agent_message") {
                        if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                            return Some(text.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Match the codex "session not found" surface documented in Task 1's
/// contract gate (`docs/decisions/2026-05-17-codex-session-capture.md`).
///
/// Detection rule: `exit != 0` AND `stderr.contains("no rollout found for
/// thread id")`. The exit-code check is done by the caller; this function
/// only checks the stderr substring.
fn looks_like_session_lost(stderr: &str) -> bool {
    stderr.contains("no rollout found for thread id")
}

#[cfg(all(test, unix))]
mod followup_tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    /// Process-wide async mutex to serialize PATH-touching tests for the full
    /// duration of each test — including across `.await` points. A
    /// `std::sync::Mutex` cannot be held across `.await` (clippy
    /// `await_holding_lock`); `tokio::sync::Mutex` is the correct tool here.
    /// Without this serialization, two parallel `#[tokio::test]` futures can
    /// race on the PATH env var and observe each other's mock binary.
    static PATH_LOCK: Mutex<()> = Mutex::const_new(());

    /// Build a fake `codex` binary in a tempdir, point PATH at it, and
    /// return a guard that restores PATH on drop. The script behavior is
    /// determined by the inline body the caller provides.
    struct PathGuard {
        _dir: tempfile::TempDir,
        original_path: String,
    }
    impl Drop for PathGuard {
        fn drop(&mut self) {
            env::set_var("PATH", &self.original_path);
        }
    }

    fn setup_mock_codex(script_body: &str) -> PathGuard {
        let dir = tempdir().unwrap();
        let codex_path = dir.path().join("codex");
        fs::write(&codex_path, script_body).unwrap();
        let mut perms = fs::metadata(&codex_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&codex_path, perms).unwrap();
        let original_path = env::var("PATH").unwrap_or_default();
        env::set_var(
            "PATH",
            format!("{}:{}", dir.path().display(), original_path),
        );
        PathGuard {
            _dir: dir,
            original_path,
        }
    }

    #[tokio::test]
    async fn followup_resume_happy_path() {
        // Hold the async PATH_LOCK for the entire test — including across
        // the `.await` on `followup()` — so the other test cannot swap in
        // its own mock binary while our subprocess is running.
        // `tokio::sync::Mutex` is await-safe; clippy's `await_holding_lock`
        // lint only fires for `std::sync::Mutex`.
        let _path_lock = PATH_LOCK.lock().await;
        let script = r#"#!/bin/sh
echo '{"type":"thread.started","thread_id":"01HFAKE12345"}'
echo '{"type":"agent_message","item":{"text":"the response body"}}'
"#;
        let _guard = setup_mock_codex(script);
        let p = CodexSubprocessProvider;
        let r = p
            .followup(Some("01HFAKE00001"), "what changed?", "sys", "gpt-5.5", &[])
            .await
            .unwrap();
        assert!(
            r.text.contains("the response body"),
            "text was: {:?}",
            r.text
        );
        assert_eq!(r.session_id.as_deref(), Some("01HFAKE12345"));
        assert!(r.resumed);
    }

    #[tokio::test]
    async fn followup_session_lost_falls_back_to_replay() {
        // Same rationale: hold the async PATH_LOCK for the full async body.
        let _path_lock = PATH_LOCK.lock().await;
        let script = r#"#!/bin/sh
case "$1 $2" in
  "exec resume")
    echo "Error: thread/resume: thread/resume failed: no rollout found for thread id 01HDEAD00000 (code -32600)" 1>&2
    exit 1
    ;;
esac
echo '{"type":"thread.started","thread_id":"01HFRESH00000"}'
echo '{"type":"agent_message","item":{"text":"replayed response body"}}'
"#;
        let _guard = setup_mock_codex(script);
        let p = CodexSubprocessProvider;
        let r = p
            .followup(Some("01HDEAD00000"), "what changed?", "sys", "gpt-5.5", &[])
            .await
            .unwrap();
        assert!(
            r.text.contains("replayed response body"),
            "text was: {:?}",
            r.text
        );
        assert_eq!(r.session_id.as_deref(), Some("01HFRESH00000"));
        assert!(!r.resumed); // fell back to non-resume path
    }
}

#[cfg(test)]
mod default_model_tests {
    use super::*;

    /// Drift guard: the documented Codex default in README.md and CLAUDE.md is
    /// `gpt-5.5`. If this const changes without updating the docs (or vice
    /// versa), `cargo test` fails instead of silently reintroducing the
    /// `gpt-5-codex` 4xx regression we hit in May 2026.
    #[test]
    fn default_codex_model_matches_documented_value() {
        assert_eq!(DEFAULT_CODEX_MODEL, "gpt-5.5");
    }
}
