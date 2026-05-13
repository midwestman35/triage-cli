//! Claude provider — subprocess to the `claude` CLI.
//!
//! Python uses the `claude-agent-sdk` library. The Rust port shells out
//! to the `claude` binary (must be installed and authenticated). See
//! `REGRESSIONS.md` R1 for the streaming/latency caveat.

use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::{LlmProvider, ProviderError};

pub struct ClaudeSubprocessProvider;

impl LlmProvider for ClaudeSubprocessProvider {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn complete<'a>(
        &'a self,
        prompt: &'a str,
        system_prompt: &'a str,
        model: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            if which::which("claude").is_err() {
                return Err(ProviderError::SubprocessMissing("claude"));
            }
            // `claude --print` runs a one-shot prompt and exits. The prompt
            // body comes in on stdin so we don't blow ARG_MAX on long inputs;
            // `--system-prompt` overrides the default; `--model` selects the
            // model. `--output-format text` is the default and pipes stdout.
            let mut child = Command::new("claude")
                .arg("--print")
                .arg("--system-prompt")
                .arg(system_prompt)
                .arg("--model")
                .arg(model)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| ProviderError::SubprocessFailure("claude", e.to_string()))?;
            if let Some(stdin) = child.stdin.as_mut() {
                stdin
                    .write_all(prompt.as_bytes())
                    .await
                    .map_err(|e| ProviderError::SubprocessFailure("claude", e.to_string()))?;
            }
            // Drop stdin handle so the child sees EOF and proceeds.
            drop(child.stdin.take());

            let output = child
                .wait_with_output()
                .await
                .map_err(|e| ProviderError::SubprocessFailure("claude", e.to_string()))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                return Err(ProviderError::SubprocessFailure(
                    "claude",
                    format!("exit {:?}: {}", output.status.code(), stderr.trim()),
                ));
            }
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            Ok(stdout)
        })
    }
}
