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
