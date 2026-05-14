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

use super::{LlmProvider, ProviderError};

/// Default model passed via `codex exec --model` when `CODEX_MODEL` is unset.
/// Single source of truth — `llm::model_for_provider` references this.
pub const DEFAULT_CODEX_MODEL: &str = "gpt-5-codex";

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
    ) -> Pin<Box<dyn Future<Output = Result<String, ProviderError>> + Send + 'a>> {
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
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            Ok(stdout)
        })
    }
}
