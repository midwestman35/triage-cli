//! Read-only Datadog Logs v2 client.
//!
//! Per the project goal, this implementation talks directly to the Datadog HTTP
//! API rather than going through `datadog-api-client`. The endpoint, query
//! shape, status-code mapping, and 200-line truncation flag all match the
//! Python reference.

use std::collections::HashSet;
use std::env;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::{Client, StatusCode};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::models::LogLine;

const DEFAULT_MAX_LINES: u32 = 200;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_SITE: &str = "datadoghq.com";
const DEFAULT_CALL_CENTER_TAG: &str = "@log.machineData.callCenterName";

static SAFE_SITE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-zA-Z0-9._-]+$").unwrap());

fn valid_levels() -> HashSet<&'static str> {
    HashSet::from(["error", "warn", "info", "debug"])
}

#[derive(Debug, Error)]
pub enum DatadogError {
    #[error("Missing required environment variables: {0}")]
    MissingEnv(String),
    #[error("levels list cannot be empty")]
    EmptyLevels,
    #[error("site_name cannot be empty")]
    EmptySiteName,
    #[error(
        "site_name {0:?} contains characters that are unsafe for Datadog query interpolation; \
         expected only [a-zA-Z0-9._-]"
    )]
    UnsafeSiteName(String),
    #[error("start must be strictly before end")]
    InvalidWindow,
    #[error("Invalid log levels: {0:?}. Valid: [\"debug\", \"error\", \"info\", \"warn\"]")]
    InvalidLevels(Vec<String>),
    #[error("Datadog auth failed — check DD_API_KEY and DD_APP_KEY")]
    AuthFailed,
    #[error("Datadog API error {0}: {1}")]
    HttpStatus(u16, String),
    #[error("Datadog request failed: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("Datadog returned malformed JSON: {0}")]
    Decode(#[source] serde_json::Error),
}

pub struct DatadogClient {
    client: Client,
    api_key: String,
    app_key: String,
    site: String,
    call_center_tag: String,
    max_lines: u32,
}

impl DatadogClient {
    pub fn new(
        api_key: String,
        app_key: String,
        site: String,
        call_center_tag: String,
        max_lines: u32,
    ) -> Result<Self, DatadogError> {
        let client = Client::builder()
            .user_agent("triage-cli/0.1")
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()?;
        Ok(Self {
            client,
            api_key,
            app_key,
            site,
            call_center_tag,
            max_lines,
        })
    }

    pub fn from_env() -> Result<Self, DatadogError> {
        let api_key = env::var("DD_API_KEY").unwrap_or_default();
        let app_key = env::var("DD_APP_KEY").unwrap_or_default();
        let mut missing = Vec::new();
        if api_key.is_empty() {
            missing.push("DD_API_KEY");
        }
        if app_key.is_empty() {
            missing.push("DD_APP_KEY");
        }
        if !missing.is_empty() {
            return Err(DatadogError::MissingEnv(missing.join(", ")));
        }
        Self::new(
            api_key,
            app_key,
            env::var("DD_SITE")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_SITE.into()),
            env::var("DD_CALL_CENTER_TAG")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_CALL_CENTER_TAG.into()),
            DEFAULT_MAX_LINES,
        )
    }

    /// Fetch logs in the window. Returns `(chronological logs, truncated_bool)`.
    pub async fn get_logs(
        &self,
        site_name: &str,
        levels: &[String],
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<(Vec<LogLine>, bool), DatadogError> {
        if levels.is_empty() {
            return Err(DatadogError::EmptyLevels);
        }
        let clean_site = site_name.trim();
        if clean_site.is_empty() {
            return Err(DatadogError::EmptySiteName);
        }
        if !SAFE_SITE_RE.is_match(clean_site) {
            return Err(DatadogError::UnsafeSiteName(clean_site.to_string()));
        }
        if start >= end {
            return Err(DatadogError::InvalidWindow);
        }
        let norm_levels: Vec<String> = levels
            .iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .collect();
        let valid = valid_levels();
        let invalid: Vec<String> = norm_levels
            .iter()
            .filter(|l| !valid.contains(l.as_str()))
            .cloned()
            .collect();
        if !invalid.is_empty() {
            return Err(DatadogError::InvalidLevels(invalid));
        }

        let query = format!(
            "{}:{} status:({})",
            self.call_center_tag,
            clean_site,
            norm_levels.join(" OR ")
        );

        let url = format!("https://api.{}/api/v2/logs/events", self.site);
        let resp = self
            .client
            .get(&url)
            .header("DD-API-KEY", &self.api_key)
            .header("DD-APPLICATION-KEY", &self.app_key)
            .header(reqwest::header::ACCEPT, "application/json")
            .query(&[
                ("filter[query]", query.as_str()),
                (
                    "filter[from]",
                    &start.to_rfc3339_opts(SecondsFormat::Millis, true),
                ),
                (
                    "filter[to]",
                    &end.to_rfc3339_opts(SecondsFormat::Millis, true),
                ),
                ("sort", "timestamp"),
                ("page[limit]", &self.max_lines.to_string()),
            ])
            .send()
            .await?;

        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(DatadogError::AuthFailed);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(200).collect();
            return Err(DatadogError::HttpStatus(status.as_u16(), snippet));
        }

        let json: Value = resp.json().await.map_err(DatadogError::Transport)?;
        let data = json
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut logs: Vec<LogLine> = data.iter().map(to_log_line).collect();
        logs.sort_by_key(|l| l.timestamp);
        let truncated = logs.len() as u32 >= self.max_lines;
        Ok((logs, truncated))
    }
}

fn to_log_line(item: &Value) -> LogLine {
    // `item.attributes` is the outer object; the v2 schema then nests another
    // `attributes` map inside it for the structured fields.
    let outer = item.get("attributes").cloned().unwrap_or(Value::Null);
    let inner = outer.get("attributes").cloned().unwrap_or(Value::Null);

    let ts_raw = outer
        .get("timestamp")
        .or_else(|| inner.get("timestamp"))
        .cloned()
        .unwrap_or(Value::Null);
    let timestamp = match &ts_raw {
        Value::String(s) => DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        Value::Number(n) => n
            .as_i64()
            .and_then(|ms| DateTime::<Utc>::from_timestamp_millis(ms))
            .unwrap_or_else(Utc::now),
        _ => Utc::now(),
    };

    let level = inner
        .get("status")
        .or_else(|| outer.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("info")
        .to_ascii_lowercase();
    let message = outer
        .get("message")
        .or_else(|| inner.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let attributes = inner.as_object().cloned().unwrap_or_else(Map::new);

    LogLine {
        timestamp,
        level,
        message,
        attributes,
    }
}
