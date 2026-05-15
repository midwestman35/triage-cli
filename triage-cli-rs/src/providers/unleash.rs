//! Unleash LLM provider — POST to `/chats`. Mirrors Python `providers.unleash`.

use std::env;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::Client;
use serde_json::{json, Value};

use super::{base_url, required_env, CompletionResult, LlmProvider, ProviderError};

const DEFAULT_BASE_URL: &str = "https://e-api.unleash.so";
const LLM_TIMEOUT_SECS: u64 = 90;

pub struct UnleashProvider;

impl LlmProvider for UnleashProvider {
    fn name(&self) -> &'static str {
        "unleash"
    }

    fn complete<'a>(
        &'a self,
        prompt: &'a str,
        system_prompt: &'a str,
        _model: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<CompletionResult, ProviderError>> + Send + 'a>> {
        Box::pin(async move {
            let endpoint = format!("{}/chats", base_url("UNLEASH_BASE_URL", DEFAULT_BASE_URL));
            let headers = unleash_headers()?;
            let assistant_id = required_env("UNLEASH_ASSISTANT_ID", "unleash")?;
            let payload = json!({
                "assistantId": assistant_id,
                "stream": false,
                "messages": [
                    {"role": "System", "text": system_prompt},
                    {"role": "User", "text": prompt},
                ],
            });
            let (data, response_headers) = post_json(&endpoint, headers, payload).await?;
            let text = unleash_text_from_response(&data);
            if text.is_empty() {
                let rid = request_id_from_payload(&data)
                    .or_else(|| response_headers.get("requestid").cloned())
                    .map(|r| format!(" RequestId: {r}."))
                    .unwrap_or_default();
                return Err(ProviderError::NoText("Unleash", rid));
            }
            let (tokens_in, tokens_out) = unleash_token_counts(&data);
            Ok(CompletionResult {
                text,
                tokens_in,
                tokens_out,
            })
        })
    }
}

fn unleash_headers() -> Result<Vec<(String, String)>, ProviderError> {
    let api_key = required_env("UNLEASH_API_KEY", "unleash")?;
    let lowered = api_key.to_ascii_lowercase();
    let authorization = if lowered.starts_with("bearer ") {
        api_key.clone()
    } else if lowered.starts_with("bearer:") {
        let after = api_key.split_once(':').map(|x| x.1).unwrap_or("").trim();
        format!("Bearer {after}")
    } else {
        format!("Bearer {api_key}")
    };
    let mut headers = vec![
        ("Authorization".into(), authorization),
        ("Accept".into(), "application/json".into()),
        ("Content-Type".into(), "application/json".into()),
    ];
    let account = env::var("UNLEASH_ACCOUNT").unwrap_or_default();
    let account = account.trim();
    if !account.is_empty() {
        headers.push(("unleash-account".into(), account.to_string()));
    }
    Ok(headers)
}

fn unleash_text_from_response(data: &Value) -> String {
    match data {
        Value::Array(items) => items.iter().map(unleash_text_from_response).collect(),
        Value::Object(_) => {
            let message = data.get("message").cloned().unwrap_or(Value::Null);
            let parts = message
                .get("parts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut chunks = Vec::with_capacity(parts.len());
            for part in parts {
                if part.get("type").and_then(Value::as_str) == Some("Text") {
                    if let Some(s) = part.get("text").and_then(Value::as_str) {
                        chunks.push(s.to_string());
                    }
                }
            }
            chunks.join("")
        }
        _ => String::new(),
    }
}

/// Extract token usage counts from the Unleash response when present.
/// Tries common field paths used by LLM gateway APIs; returns `(None, None)`
/// if the fields are absent (non-fatal — token counts are best-effort).
fn unleash_token_counts(data: &Value) -> (Option<u32>, Option<u32>) {
    // Try top-level `usage` object (e.g. { "usage": { "input_tokens": N, "output_tokens": N } })
    if let Some(usage) = data.get("usage") {
        let tin = usage
            .get("input_tokens")
            .or_else(|| usage.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .map(|n| n as u32);
        let tout = usage
            .get("output_tokens")
            .or_else(|| usage.get("completion_tokens"))
            .and_then(Value::as_u64)
            .map(|n| n as u32);
        if tin.is_some() || tout.is_some() {
            return (tin, tout);
        }
    }
    // Try `message.usage` (some gateway shapes nest it there)
    if let Some(msg_usage) = data.get("message").and_then(|m| m.get("usage")) {
        let tin = msg_usage
            .get("input_tokens")
            .or_else(|| msg_usage.get("prompt_tokens"))
            .and_then(Value::as_u64)
            .map(|n| n as u32);
        let tout = msg_usage
            .get("output_tokens")
            .or_else(|| msg_usage.get("completion_tokens"))
            .and_then(Value::as_u64)
            .map(|n| n as u32);
        if tin.is_some() || tout.is_some() {
            return (tin, tout);
        }
    }
    (None, None)
}

fn request_id_from_payload(data: &Value) -> Option<String> {
    data.get("requestId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

pub(crate) async fn post_json(
    endpoint: &str,
    headers: Vec<(String, String)>,
    payload: Value,
) -> Result<(Value, std::collections::HashMap<String, String>), ProviderError> {
    let client = Client::builder()
        .timeout(Duration::from_secs(LLM_TIMEOUT_SECS))
        .build()
        .map_err(|e| ProviderError::Transport(e.to_string()))?;
    let mut req = client.post(endpoint);
    for (k, v) in headers {
        req = req.header(&k, &v);
    }
    let resp = req
        .json(&payload)
        .send()
        .await
        .map_err(|e| ProviderError::Transport(e.to_string()))?;
    let status = resp.status();
    let response_headers: std::collections::HashMap<String, String> = resp
        .headers()
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|s| (k.as_str().to_ascii_lowercase(), s.to_string()))
        })
        .collect();
    if status.as_u16() >= 400 {
        return Err(provider_error(status.as_u16(), resp).await);
    }
    let text = resp
        .text()
        .await
        .map_err(|e| ProviderError::Transport(e.to_string()))?;
    let data: Value = serde_json::from_str(&text).map_err(|_| ProviderError::NonJson)?;
    Ok((data, response_headers))
}

async fn provider_error(status: u16, resp: reqwest::Response) -> ProviderError {
    let rid = resp
        .headers()
        .get("RequestId")
        .or_else(|| resp.headers().get("x-request-id"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let text = resp.text().await.unwrap_or_default();
    let mut detail = text.trim().to_string();
    if let Ok(parsed) = serde_json::from_str::<Value>(&detail) {
        if let Some(dv) = parsed
            .get("message")
            .or_else(|| parsed.get("error"))
            .or_else(|| parsed.get("detail"))
        {
            detail = match dv {
                Value::String(s) => s.clone(),
                Value::Object(_) => dv
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| dv.to_string()),
                _ => dv.to_string(),
            };
        }
    }
    let rid_suffix = match rid {
        Some(r) => format!(" RequestId: {r}."),
        None => String::new(),
    };
    let detail_suffix = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };
    ProviderError::HttpStatus {
        status,
        detail: detail_suffix,
        rid: rid_suffix,
    }
}
