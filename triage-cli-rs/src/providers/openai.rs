//! OpenAI Responses API provider.
//!
//! Hits the `/responses` endpoint, treating `system_prompt` as
//! `instructions` and `prompt` as `input`. Behavior tracks the Python
//! implementation; output text is extracted from `output_text` (or the
//! nested `output[].content[].text` array if the top-level shortcut is
//! absent).

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::Client;
use serde_json::{json, Value};

use super::{base_url, required_env, LlmProvider, ProviderError};
use super::unleash::{provider_error_message, LLM_TIMEOUT_SECS};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

pub struct OpenAiProvider;

impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn complete<'a>(
        &'a self,
        prompt: &'a str,
        system_prompt: &'a str,
        model: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            let endpoint = format!("{}/responses", base_url("OPENAI_BASE_URL", DEFAULT_BASE_URL));
            let api_key = required_env("OPENAI_API_KEY", "openai")?;
            let payload = json!({
                "model": model,
                "instructions": system_prompt,
                "input": prompt,
                "store": false,
            });

            let client = Client::builder()
                .timeout(Duration::from_secs(LLM_TIMEOUT_SECS))
                .build()
                .map_err(|e| ProviderError::Transport(e.to_string()))?;
            let resp = client
                .post(&endpoint)
                .header("Authorization", format!("Bearer {api_key}"))
                .header("Accept", "application/json")
                .header("Content-Type", "application/json")
                .json(&payload)
                .send()
                .await
                .map_err(|e| ProviderError::Transport(e.to_string()))?;
            let status = resp.status();
            let response_headers = resp.headers().clone();
            if status.as_u16() >= 400 {
                let rid = response_headers
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                let body = resp.text().await.unwrap_or_default();
                return Err(provider_error_message(status.as_u16(), &body, rid.as_deref()));
            }
            let body = resp
                .text()
                .await
                .map_err(|e| ProviderError::Transport(e.to_string()))?;
            let data: Value = serde_json::from_str(&body).map_err(|_| ProviderError::NonJson)?;
            let text = openai_text_from_response(&data);
            if text.is_empty() {
                let rid = openai_request_id(&data)
                    .or_else(|| {
                        response_headers
                            .get("x-request-id")
                            .and_then(|v| v.to_str().ok())
                            .map(|s| s.to_string())
                    })
                    .map(|r| format!(" RequestId: {r}."))
                    .unwrap_or_default();
                return Err(ProviderError::NoText("OpenAI Responses", rid));
            }
            Ok(text)
        })
    }
}

fn openai_text_from_response(data: &Value) -> String {
    if let Some(top) = data.get("output_text").and_then(Value::as_str) {
        return top.to_string();
    }
    let Some(output) = data.get("output").and_then(Value::as_array) else {
        return String::new();
    };
    let mut chunks: Vec<String> = Vec::new();
    for item in output {
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                if let Some(s) = part.get("text").and_then(Value::as_str) {
                    chunks.push(s.to_string());
                }
            }
        }
    }
    chunks.join("")
}

fn openai_request_id(data: &Value) -> Option<String> {
    data.get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}
