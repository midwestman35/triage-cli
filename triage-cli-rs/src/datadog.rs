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
        self.get_logs_inner(site_name, levels, start, end).await
    }

    async fn get_logs_inner(
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
        let logs = parse_log_lines(&data);
        let truncated = logs.len() as u32 >= self.max_lines;
        Ok((logs, truncated))
    }
}

/// Parse a JSON array of Datadog v2 log items into chronologically-sorted
/// `LogLine`s. Entries with a missing, non-string/-number, or unparseable
/// timestamp are silently dropped — `to_log_line` returning `None` is the
/// signal. F5: this used to fall back to `Utc::now()`, which both
/// fabricated evidence timing *and* re-sorted the fabricated row to the
/// top of the result via `sort_by_key`.
pub(crate) fn parse_log_lines(data: &[Value]) -> Vec<LogLine> {
    let mut logs: Vec<LogLine> = data.iter().filter_map(to_log_line).collect();
    logs.sort_by_key(|l| l.timestamp);
    logs
}

fn to_log_line(item: &Value) -> Option<LogLine> {
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
            .ok()?,
        Value::Number(n) => n
            .as_i64()
            .and_then(DateTime::<Utc>::from_timestamp_millis)?,
        _ => return None,
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

    Some(LogLine {
        timestamp,
        level,
        message,
        attributes,
    })
}

/// Boxed future returned by [`DatadogSource::get_logs`].
pub type LogsFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(Vec<LogLine>, bool), DatadogError>> + Send + 'a>,
>;

/// Abstracts over the real Datadog client and the fixture stub so the pipeline
/// can be wired with either without knowing which it has.
///
/// Uses the explicit `Pin<Box<dyn Future>>` form (rather than `async fn`) to
/// remain dyn-compatible, following the same pattern as `LlmProvider`.
pub trait DatadogSource: Send + Sync {
    fn get_logs<'a>(
        &'a self,
        site_name: &'a str,
        levels: &'a [String],
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> LogsFuture<'a>;
}

impl DatadogSource for DatadogClient {
    fn get_logs<'a>(
        &'a self,
        site_name: &'a str,
        levels: &'a [String],
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> LogsFuture<'a> {
        Box::pin(self.get_logs_inner(site_name, levels, start, end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- to_log_line: Datadog v2 JSON → LogLine mapping ---------------------

    #[test]
    fn to_log_line_parses_rfc3339_string_timestamp_from_outer() {
        let item = json!({
            "attributes": {
                "timestamp": "2026-05-16T08:30:00.123Z",
                "status": "ERROR",
                "message": "console offline"
            }
        });
        let line = to_log_line(&item).expect("valid RFC3339 timestamp must parse");
        assert_eq!(
            line.timestamp,
            DateTime::parse_from_rfc3339("2026-05-16T08:30:00.123Z")
                .unwrap()
                .with_timezone(&Utc)
        );
        // status is lowercased.
        assert_eq!(line.level, "error");
        assert_eq!(line.message, "console offline");
        assert!(line.attributes.is_empty());
    }

    #[test]
    fn to_log_line_parses_numeric_millis_timestamp() {
        // 2026-05-16T08:30:00Z == 1_778_999_400_000 ms; assert round-trip.
        let expected = DateTime::parse_from_rfc3339("2026-05-16T08:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let millis = expected.timestamp_millis();
        let item = json!({
            "attributes": {
                "timestamp": millis,
                "status": "warn",
                "message": "retrying"
            }
        });
        let line = to_log_line(&item).expect("valid millis timestamp must parse");
        assert_eq!(line.timestamp, expected);
        assert_eq!(line.level, "warn");
    }

    #[test]
    fn to_log_line_respects_field_precedence_between_outer_and_inner() {
        // Code precedence: timestamp outer>inner, level inner>outer,
        // message outer>inner.
        let item = json!({
            "attributes": {
                "timestamp": "2026-05-16T00:00:00Z",
                "status": "INFO",
                "message": "outer-message",
                "attributes": {
                    "timestamp": "2099-01-01T00:00:00Z",
                    "status": "Error",
                    "message": "inner-message",
                    "host": "cnc-7"
                }
            }
        });
        let line = to_log_line(&item).expect("valid outer timestamp must parse");
        assert_eq!(
            line.timestamp,
            DateTime::parse_from_rfc3339("2026-05-16T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            "outer timestamp must win"
        );
        assert_eq!(line.level, "error", "inner status must win, lowercased");
        assert_eq!(line.message, "outer-message", "outer message must win");
        // Inner object becomes the attributes map.
        assert_eq!(
            line.attributes.get("host").and_then(Value::as_str),
            Some("cnc-7")
        );
    }

    #[test]
    fn to_log_line_defaults_level_and_message_when_absent() {
        let item = json!({ "attributes": { "timestamp": "2026-05-16T08:30:00Z" } });
        let line = to_log_line(&item).expect("valid timestamp must parse");
        assert_eq!(line.level, "info");
        assert_eq!(line.message, "");
        assert!(line.attributes.is_empty());
    }

    /// F5: previously this fell back to `Utc::now()`, and the subsequent
    /// `sort_by_key(timestamp)` reordered the fabricated row to the top of
    /// the result set. The pipeline then handed it to the LLM as if it
    /// were a real entry in the queried window. The contract is now:
    /// malformed timestamps drop the entry entirely.
    #[test]
    fn to_log_line_returns_none_on_missing_or_invalid_timestamp() {
        for ts in [json!("not-a-timestamp"), Value::Null, json!(true)] {
            let item = json!({ "attributes": { "timestamp": ts, "status": "info" } });
            assert!(
                to_log_line(&item).is_none(),
                "invalid timestamp {ts:?} should drop the entry, not fabricate one"
            );
        }
    }

    #[test]
    fn to_log_line_handles_completely_empty_item() {
        // No timestamp anywhere → must drop, not fabricate Utc::now().
        assert!(to_log_line(&Value::Null).is_none());
    }

    #[test]
    fn parse_log_lines_filters_malformed_and_sorts_remainder() {
        let data = vec![
            json!({ "attributes": { "timestamp": "2026-05-14T15:32:30Z", "status": "error", "message": "B" } }),
            json!({ "attributes": { "timestamp": "not-a-date", "status": "error", "message": "DROP_ME" } }),
            json!({ "attributes": { "timestamp": "2026-05-14T15:32:00Z", "status": "warn", "message": "A" } }),
        ];
        let out = parse_log_lines(&data);
        assert_eq!(out.len(), 2, "malformed entry must be filtered out");
        assert_eq!(
            out[0].message, "A",
            "results must remain chronologically sorted"
        );
        assert_eq!(out[1].message, "B");
        assert!(
            !out.iter().any(|l| l.message == "DROP_ME"),
            "malformed entry leaked into result"
        );
    }

    // --- input validation: query-injection guard + arg checks --------------
    //
    // Every case below returns before any network call, so these tests do no
    // I/O. A tiny current-thread runtime drives the async fn.

    fn client() -> DatadogClient {
        DatadogClient::new(
            "api".into(),
            "app".into(),
            "datadoghq.com".into(),
            DEFAULT_CALL_CENTER_TAG.into(),
            DEFAULT_MAX_LINES,
        )
        .expect("client build is offline")
    }

    fn validate(
        site: &str,
        levels: &[&str],
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<(Vec<LogLine>, bool), DatadogError> {
        let lv: Vec<String> = levels.iter().map(|s| s.to_string()).collect();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(client().get_logs_inner(site, &lv, start, end))
    }

    fn window() -> (DateTime<Utc>, DateTime<Utc>) {
        let start = DateTime::parse_from_rfc3339("2026-05-16T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        (start, start + chrono::Duration::minutes(30))
    }

    #[test]
    fn empty_levels_rejected() {
        let (s, e) = window();
        assert!(matches!(
            validate("site-a", &[], s, e),
            Err(DatadogError::EmptyLevels)
        ));
    }

    #[test]
    fn blank_site_name_rejected() {
        let (s, e) = window();
        assert!(matches!(
            validate("   ", &["error"], s, e),
            Err(DatadogError::EmptySiteName)
        ));
    }

    #[test]
    fn query_injection_attempts_are_rejected_by_safe_site_regex() {
        let (s, e) = window();
        // Each payload tries to break out of the `tag:<site>` clause.
        let payloads = [
            "site-a OR *",
            "site) OR (status:error",
            "site:* ",
            "site name with spaces",
            "site\nstatus:error",
            "site\"quoted\"",
            "site*",
            "../etc/passwd",
            "site;drop",
        ];
        for p in payloads {
            match validate(p, &["error"], s, e) {
                Err(DatadogError::UnsafeSiteName(got)) => {
                    assert_eq!(got, p.trim());
                }
                other => panic!("payload {p:?} should be rejected, got {other:?}"),
            }
        }
    }

    #[test]
    fn safe_site_names_pass_the_regex_guard() {
        // These must NOT trip UnsafeSiteName. We force an InvalidWindow so the
        // fn still returns before any network call.
        let (s, _e) = window();
        for ok in ["site-a", "Site_B.1", "abc123", "a.b-c_d"] {
            let err = validate(ok, &["error"], s, s).unwrap_err();
            assert!(
                matches!(err, DatadogError::InvalidWindow),
                "safe site {ok:?} should pass regex and fail later on window, got {err:?}"
            );
        }
    }

    #[test]
    fn inverted_or_empty_window_rejected() {
        let (s, e) = window();
        // start == end and start > end both invalid.
        assert!(matches!(
            validate("site-a", &["error"], s, s),
            Err(DatadogError::InvalidWindow)
        ));
        assert!(matches!(
            validate("site-a", &["error"], e, s),
            Err(DatadogError::InvalidWindow)
        ));
    }

    #[test]
    fn invalid_log_levels_rejected_and_reported() {
        let (s, e) = window();
        match validate("site-a", &["error", "critical", "trace"], s, e) {
            Err(DatadogError::InvalidLevels(bad)) => {
                assert_eq!(bad, vec!["critical".to_string(), "trace".to_string()]);
            }
            other => panic!("expected InvalidLevels, got {other:?}"),
        }
    }

    #[test]
    fn levels_are_case_and_whitespace_normalized_before_validation() {
        let (s, _e) = window();
        // Mixed case + padding should normalize to valid levels and then fail
        // on the window check (proving they passed level validation).
        let err = validate("site-a", &[" ERROR ", "Warn"], s, s).unwrap_err();
        assert!(
            matches!(err, DatadogError::InvalidWindow),
            "normalized levels should be accepted, got {err:?}"
        );
    }

    #[test]
    fn validation_check_order_levels_before_site() {
        let (s, e) = window();
        // Empty levels AND unsafe site: levels check runs first.
        assert!(matches!(
            validate("bad site*", &[], s, e),
            Err(DatadogError::EmptyLevels)
        ));
    }
}
