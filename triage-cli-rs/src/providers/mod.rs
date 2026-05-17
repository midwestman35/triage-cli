//! LLM provider abstraction. Variant selected via `LLM_PROVIDER` env var.
//!
//! - `unleash` (default): HTTP to Unleash gateway (`/chats`)
//! - `codex`: subprocess to `codex exec` (new — no Python equivalent; see
//!   `REGRESSIONS.md` R2)

pub mod codex;
pub mod unleash;

use std::env;
use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

/// Returned by `LlmProvider::complete`. Carries the assistant text and, when
/// the provider exposes it, the token counts for the call.
#[derive(Debug, Clone, Default)]
pub struct CompletionResult {
    pub text: String,
    pub tokens_in: Option<u32>,
    pub tokens_out: Option<u32>,
}

/// Returned by `LlmProvider::followup`. Extends `CompletionResult` with
/// session information for resumable providers (codex).
#[derive(Debug, Clone, Default)]
pub struct FollowupResult {
    pub text: String,
    pub tokens_in: Option<u32>,
    pub tokens_out: Option<u32>,
    pub session_id: Option<String>,
    pub resumed: bool,
}

/// Single-turn LLM completion contract. Mirrors Python `LLMProvider.complete`.
///
/// Rust 1.95 supports `async fn` in traits natively. We use the explicit
/// `dyn`-compatible form (returns `Pin<Box<dyn Future ...>>`) so the dispatch
/// site can hold the provider behind a trait object — needed because the
/// concrete provider is selected at runtime from `LLM_PROVIDER`.
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn complete<'a>(
        &'a self,
        prompt: &'a str,
        system_prompt: &'a str,
        model: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionResult, ProviderError>> + Send + 'a>>;

    /// Optional follow-up surface (spec § 5.7). Default impl ignores
    /// `session_id` and calls `complete()` with the caller-supplied
    /// replay-context prompt. Providers with native session resume
    /// (codex — Task 10) override this method.
    fn followup<'a>(
        &'a self,
        _session_id: Option<&'a str>,
        prompt: &'a str,
        system_prompt: &'a str,
        model: &'a str,
        _attachments: &'a [crate::models::Attachment],
    ) -> Pin<Box<dyn Future<Output = Result<FollowupResult, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            let r = self.complete(prompt, system_prompt, model).await?;
            Ok(FollowupResult {
                text: r.text,
                tokens_in: r.tokens_in,
                tokens_out: r.tokens_out,
                session_id: None,
                resumed: false,
            })
        })
    }
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("{0} must be set when LLM_PROVIDER={1}.")]
    MissingEnv(&'static str, &'static str),
    #[error("LLM provider API call failed: {0}")]
    Transport(String),
    #[error("LLM provider API response was not valid JSON.")]
    NonJson,
    #[error("LLM provider API call failed with HTTP {status}{detail}.{rid}")]
    HttpStatus {
        status: u16,
        detail: String,
        rid: String,
    },
    #[error("{0} provider response did not include any assistant text.{1}")]
    NoText(&'static str, String),
    #[error("Unknown LLM_PROVIDER: {0:?}. Valid: unleash, codex")]
    Unknown(String),
    #[error("subprocess {0} not found on PATH")]
    SubprocessMissing(&'static str),
    #[error("subprocess {0} failed: {1}")]
    SubprocessFailure(&'static str, String),
}

/// Return the configured LLM provider.
pub fn get_provider() -> Result<Box<dyn LlmProvider>, ProviderError> {
    let raw = env::var("LLM_PROVIDER").unwrap_or_else(|_| "unleash".into());
    match raw.to_ascii_lowercase().as_str() {
        "unleash" => Ok(Box::new(unleash::UnleashProvider)),
        "codex" => Ok(Box::new(codex::CodexSubprocessProvider)),
        _ => Err(ProviderError::Unknown(raw)),
    }
}

pub(crate) fn required_env(
    name: &'static str,
    provider: &'static str,
) -> Result<String, ProviderError> {
    let v = env::var(name).unwrap_or_default();
    let trimmed = v.trim();
    if trimmed.is_empty() {
        return Err(ProviderError::MissingEnv(name, provider));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn base_url(env_name: &str, default: &str) -> String {
    let raw = env::var(env_name).unwrap_or_default();
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        default.trim_end_matches('/').to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod followup_tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeProvider {
        last_prompt: Mutex<Option<String>>,
    }
    impl LlmProvider for FakeProvider {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn complete<'a>(
            &'a self,
            prompt: &'a str,
            _system: &'a str,
            _model: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<CompletionResult, ProviderError>> + Send + 'a>>
        {
            Box::pin(async move {
                *self.last_prompt.lock().unwrap() = Some(prompt.to_string());
                Ok(CompletionResult {
                    text: format!("echo:{prompt}"),
                    tokens_in: Some(10),
                    tokens_out: Some(20),
                })
            })
        }
    }

    #[tokio::test]
    async fn default_followup_uses_replay_context() {
        let p = FakeProvider {
            last_prompt: Mutex::new(None),
        };
        let r = p
            .followup(Some("ignored-session-id"), "what changed?", "sys", "m", &[])
            .await
            .unwrap();
        assert_eq!(r.text, "echo:what changed?");
        assert!(!r.resumed);
        assert!(r.session_id.is_none());
    }
}
