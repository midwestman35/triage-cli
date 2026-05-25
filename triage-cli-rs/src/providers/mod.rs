//! LLM provider abstraction. Variant selected via `LLM_PROVIDER` env var.
//!
//! - `unleash` (default): HTTP to Unleash gateway (`/chats`)
//! - `codex`: subprocess to `codex exec` (new — no Python equivalent; see
//!   `REGRESSIONS.md` R2)

pub mod codex;
pub mod codex_app_server;
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

/// Internal provider progress events (bridged to inbox `ChatEvent` in Phase 4).
#[derive(Debug, Clone)]
pub enum ProviderProgress {
    Stage { label: String },
    TextDelta { text: String },
    Error { message: String },
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

    /// Optional follow-up surface (spec § 5.7). Default impl stamps
    /// attachment content into the prompt then delegates to `complete()`.
    /// Providers with native session resume (codex — Task 10) override
    /// this method.
    fn followup<'a>(
        &'a self,
        _session_id: Option<&'a str>,
        prompt: &'a str,
        system_prompt: &'a str,
        model: &'a str,
        attachments: &'a [crate::models::Attachment],
    ) -> Pin<Box<dyn Future<Output = Result<FollowupResult, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            let stamped = stamp_attachments_into_prompt(prompt, attachments);
            let r = self.complete(&stamped, system_prompt, model).await?;
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

fn codex_transport_mode() -> CodexTransportMode {
    let raw = env::var("CODEX_TRANSPORT").unwrap_or_default();
    match raw.trim().to_ascii_lowercase().as_str() {
        "exec" => CodexTransportMode::Exec,
        "app-server" | "app_server" => CodexTransportMode::AppServer,
        "" => CodexTransportMode::AppServer,
        _ => CodexTransportMode::AppServer,
    }
}

/// Active Codex transport label for session provenance (`app-server` | `exec`).
pub fn codex_transport_label() -> &'static str {
    match codex_transport_mode() {
        CodexTransportMode::Exec => "exec",
        CodexTransportMode::AppServer => "app-server",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexTransportMode {
    AppServer,
    Exec,
}

/// Return the configured LLM provider.
pub fn get_provider() -> Result<Box<dyn LlmProvider>, ProviderError> {
    let raw = env::var("LLM_PROVIDER").unwrap_or_else(|_| "unleash".into());
    match raw.to_ascii_lowercase().as_str() {
        "unleash" => Ok(Box::new(unleash::UnleashProvider)),
        "codex" => {
            if codex_transport_mode() == CodexTransportMode::Exec {
                Ok(Box::new(codex::CodexSubprocessProvider))
            } else if codex_app_server::probe_app_server_sync() {
                Ok(Box::new(codex_app_server::CodexAppServerProvider::new()))
            } else {
                codex_app_server::log_app_server_fallback_once();
                Ok(Box::new(codex::CodexSubprocessProvider))
            }
        }
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

/// Stamp attachment content into a prompt string. Used by providers that
/// don't have a native multi-part request channel (codex subprocess,
/// unleash /chats body). Empty attachments → prompt unchanged.
pub(crate) fn stamp_attachments_into_prompt(
    prompt: &str,
    attachments: &[crate::models::Attachment],
) -> String {
    if attachments.is_empty() {
        return prompt.to_string();
    }
    let mut out = String::from(prompt);
    out.push_str("\n\n## Attached files");
    for a in attachments {
        out.push_str(&format!(
            "\n\n### file: {} ({})",
            a.basename,
            a.detected_type.as_str()
        ));
        match &a.extracted_text {
            Some(text) => {
                out.push('\n');
                out.push_str(text);
            }
            None => out.push_str("\n[binary or unreadable — content not included]"),
        }
    }
    out
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

    #[tokio::test]
    async fn default_followup_stamps_attachments_into_prompt() {
        use crate::models::{Attachment, FileType};
        let p = FakeProvider {
            last_prompt: Mutex::new(None),
        };
        let att = Attachment {
            copied_path: std::path::PathBuf::from("/dev/null"),
            basename: "diag.log".into(),
            detected_type: FileType::Log,
            extracted_text: Some("ATTACHMENT_BODY_SENTINEL".into()),
        };
        let r = p
            .followup(
                None,
                "what changed?",
                "sys",
                "m",
                std::slice::from_ref(&att),
            )
            .await
            .unwrap();
        assert!(r.text.contains("ATTACHMENT_BODY_SENTINEL"));
        let captured = p.last_prompt.lock().unwrap().clone().unwrap();
        assert!(
            captured.contains("ATTACHMENT_BODY_SENTINEL"),
            "stamp missing: {captured}"
        );
        assert!(
            captured.contains("diag.log"),
            "basename missing: {captured}"
        );
    }
}

#[cfg(test)]
mod stamp_tests {
    use super::*;

    #[test]
    fn stamp_attachments_empty_returns_prompt_unchanged() {
        assert_eq!(stamp_attachments_into_prompt("hello", &[]), "hello");
    }

    #[test]
    fn stamp_attachments_includes_basename_and_content() {
        use crate::models::{Attachment, FileType};
        let a = Attachment {
            copied_path: std::path::PathBuf::from("/x/y.log"),
            basename: "y.log".into(),
            detected_type: FileType::Log,
            extracted_text: Some("CONTENT".into()),
        };
        let s = stamp_attachments_into_prompt("question", std::slice::from_ref(&a));
        assert!(s.contains("question"));
        assert!(s.contains("y.log"));
        assert!(s.contains("CONTENT"));
    }

    #[test]
    fn stamp_attachments_handles_binary_attachment() {
        use crate::models::{Attachment, FileType};
        let a = Attachment {
            copied_path: std::path::PathBuf::from("/x/y.bin"),
            basename: "y.bin".into(),
            detected_type: FileType::Unknown,
            extracted_text: None,
        };
        let s = stamp_attachments_into_prompt("question", std::slice::from_ref(&a));
        assert!(
            s.contains("[binary or unreadable"),
            "missing fallback note: {s}"
        );
    }
}
