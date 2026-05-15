//! Read-only Zendesk client for fetching a ticket and its full comment thread.
//!
//! Ported from Python `triage_cli/zendesk.py`. Differences:
//! - All methods are async; the sync `__enter__`/`__exit__` context manager is
//!   replaced by drop-based lifetime management on the underlying `reqwest::Client`.
//! - Pagination, status-code mapping, and error wording match Python byte-for-byte
//!   so the `doctor` and Datadog error-paths surface the same messages.

use std::env;
use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{Client, RequestBuilder, StatusCode, Url};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

use crate::models::{AttachmentEvidence, Comment, Ticket, TicketSummary};

const USER_AGENT: &str = "triage-cli/0.1";
const PAGE_SIZE: u32 = 100;
const MAX_PAGES: u32 = 1000;
const DEFAULT_TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Error)]
pub enum ZendeskError {
    #[error("Missing required environment variables: {0}")]
    MissingEnv(String),
    #[error("Zendesk request failed: {0}")]
    Transport(#[source] reqwest::Error),
    #[error("Zendesk returned non-JSON response: {0}")]
    NonJson(#[source] serde_json::Error),
    #[error("Ticket {0} not found")]
    TicketNotFound(u64),
    #[error("View {0} not found")]
    ViewNotFound(u64),
    #[error("Zendesk auth failed - check ZENDESK_EMAIL and ZENDESK_API_TOKEN")]
    AuthFailed,
    #[error("Zendesk rate-limited; retry after {0} seconds")]
    RateLimited(String),
    #[error("Zendesk error {0}: {1}")]
    HttpStatus(u16, String),
    #[error("Attachment not found at {0}")]
    AttachmentNotFound(String),
    #[error("Zendesk auth failed during attachment download")]
    AttachmentAuthFailed,
    #[error("attachment is {0} bytes; cap is {1} bytes")]
    AttachmentTooLargePreflight(u64, u64),
    #[error("attachment exceeded cap {0} bytes mid-stream")]
    AttachmentTooLargeMidStream(u64),
    #[error("Attachment download failed: {0}")]
    AttachmentTransport(#[source] reqwest::Error),
    #[error("Could not determine current Zendesk user ID from /users/me.json")]
    NoCurrentUser,
    #[error("Zendesk view pagination exceeded {0} pages - possible loop")]
    PaginationLoop(u32),
    #[error("Zendesk assigned-tickets search pagination exceeded limit")]
    SearchPaginationLoop,
}

pub struct ZendeskClient {
    client: Client,
    base_url: String,
    email_username: String,
    api_token: String,
}

#[derive(Clone, Copy)]
enum NotFoundKind {
    Ticket(u64),
    View(u64),
}

impl ZendeskClient {
    pub fn new(
        subdomain: &str,
        email: &str,
        api_token: &str,
        timeout: Duration,
    ) -> Result<Self, ZendeskError> {
        let client = Client::builder()
            .user_agent(USER_AGENT)
            .timeout(timeout)
            .build()
            .map_err(ZendeskError::Transport)?;
        Ok(Self {
            client,
            base_url: format!("https://{subdomain}.zendesk.com/api/v2"),
            email_username: format!("{email}/token"),
            api_token: api_token.to_string(),
        })
    }

    /// Construct from `ZENDESK_SUBDOMAIN`, `ZENDESK_EMAIL`, `ZENDESK_API_TOKEN`.
    pub fn from_env() -> Result<Self, ZendeskError> {
        let names = ["ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN"];
        let values: Vec<(&'static str, String)> = names
            .iter()
            .map(|n| (*n, env::var(n).unwrap_or_default()))
            .collect();
        let missing: Vec<&str> = values
            .iter()
            .filter(|(_, v)| v.is_empty())
            .map(|(n, _)| *n)
            .collect();
        if !missing.is_empty() {
            return Err(ZendeskError::MissingEnv(missing.join(", ")));
        }
        Self::new(
            &values[0].1,
            &values[1].1,
            &values[2].1,
            Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        )
    }

    fn auth(&self, rb: RequestBuilder) -> RequestBuilder {
        rb.basic_auth(&self.email_username, Some(&self.api_token))
            .header(reqwest::header::ACCEPT, "application/json")
    }

    /// Fetch a Zendesk ticket plus its full comment thread.
    pub async fn get_ticket(&self, ticket_id: u64) -> Result<Ticket, ZendeskError> {
        let payload: Value = self
            .get_json(
                &format!("/tickets/{ticket_id}.json"),
                Some(&[("include", "users,organizations")]),
                NotFoundKind::Ticket(ticket_id),
            )
            .await?;

        let ticket_obj = payload.get("ticket").cloned().unwrap_or(Value::Null);
        let users_by_id = index_by_id(payload.get("users"));
        let orgs_by_id = index_by_id(payload.get("organizations"));

        let mut org_id = ticket_obj.get("organization_id").and_then(Value::as_u64);
        if org_id.is_none() {
            if let Some(requester_id) = ticket_obj.get("requester_id").and_then(Value::as_u64) {
                if let Some(user) = users_by_id.get(&requester_id) {
                    org_id = user.get("organization_id").and_then(Value::as_u64);
                }
            }
        }
        let requester_org = org_id
            .and_then(|id| orgs_by_id.get(&id))
            .and_then(|o| o.get("name"))
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        let requester_id = ticket_obj.get("requester_id").and_then(Value::as_u64);
        let requester_email = requester_id
            .and_then(|id| users_by_id.get(&id))
            .and_then(|u| u.get("email"))
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        Ok(Ticket {
            id: ticket_obj
                .get("id")
                .and_then(Value::as_u64)
                .ok_or_else(|| ZendeskError::TicketNotFound(ticket_id))?,
            subject: ticket_obj
                .get("subject")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            description: ticket_obj
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            requester_org,
            requester_email,
            tags: ticket_obj
                .get("tags")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            created_at: parse_iso(ticket_obj.get("created_at"))?,
            updated_at: Some(parse_iso(ticket_obj.get("updated_at"))?),
            comments: self.fetch_comments(ticket_id).await?,
        })
    }

    /// Return ticket IDs in the given Zendesk view, in order.
    pub async fn list_view_ticket_ids(&self, view_id: u64) -> Result<Vec<u64>, ZendeskError> {
        let mut path: Option<String> = Some(format!("/views/{view_id}/tickets.json"));
        let mut params: Option<Vec<(String, String)>> =
            Some(vec![("page[size]".into(), PAGE_SIZE.to_string())]);
        let mut ids: Vec<u64> = Vec::new();

        for _ in 0..MAX_PAGES {
            let Some(p) = path.take() else {
                return Ok(ids);
            };
            let payload = match self
                .get_json_dyn(&p, params.as_deref(), NotFoundKind::View(view_id))
                .await
            {
                Ok(v) => v,
                Err(e) => return Err(e),
            };
            if let Some(tickets) = payload.get("tickets").and_then(Value::as_array) {
                for t in tickets {
                    if let Some(id) = t.get("id").and_then(Value::as_u64) {
                        ids.push(id);
                    }
                }
            }
            path = next_page_url(&payload);
            params = None;
        }
        Err(ZendeskError::PaginationLoop(MAX_PAGES))
    }

    /// Return IDs of open tickets assigned to the authenticated user.
    pub async fn list_my_ticket_ids(&self) -> Result<Vec<u64>, ZendeskError> {
        let me = self
            .get_json("/users/me.json", None, NotFoundKind::Ticket(0))
            .await?;
        let user_id = me
            .get("user")
            .and_then(|u| u.get("id"))
            .and_then(Value::as_u64)
            .ok_or(ZendeskError::NoCurrentUser)?;

        let mut path: Option<String> = Some("/search.json".to_string());
        let query = format!("assignee_id:{user_id} status<closed type:ticket");
        let mut params: Option<Vec<(String, String)>> = Some(vec![
            ("query".into(), query),
            ("sort_by".into(), "created_at".into()),
            ("sort_order".into(), "desc".into()),
            ("page[size]".into(), PAGE_SIZE.to_string()),
        ]);
        let mut ids: Vec<u64> = Vec::new();

        for _ in 0..MAX_PAGES {
            let Some(p) = path.take() else {
                return Ok(ids);
            };
            let payload = self
                .get_json_dyn(&p, params.as_deref(), NotFoundKind::Ticket(user_id))
                .await?;
            if let Some(results) = payload.get("results").and_then(Value::as_array) {
                for r in results {
                    if let Some(id) = r.get("id").and_then(Value::as_u64) {
                        ids.push(id);
                    }
                }
            }
            path = next_page_url(&payload);
            params = None;
        }
        Err(ZendeskError::SearchPaginationLoop)
    }

    async fn fetch_comments(&self, ticket_id: u64) -> Result<Vec<Comment>, ZendeskError> {
        let mut path: Option<String> = Some(format!("/tickets/{ticket_id}/comments.json"));
        let mut params: Option<Vec<(String, String)>> = Some(vec![
            ("include".into(), "users".into()),
            ("page[size]".into(), PAGE_SIZE.to_string()),
            ("sort".into(), "created_at".into()),
        ]);
        let mut users_by_id: std::collections::HashMap<u64, Value> =
            std::collections::HashMap::new();
        let mut raw: Vec<Value> = Vec::new();

        for _ in 0..MAX_PAGES {
            let Some(p) = path.take() else {
                let mut comments: Vec<Comment> = raw
                    .iter()
                    .map(|rc| to_comment(rc, &users_by_id))
                    .collect::<Result<_, _>>()?;
                comments.sort_by_key(|c| c.created_at);
                return Ok(comments);
            };
            let payload = self
                .get_json_dyn(&p, params.as_deref(), NotFoundKind::Ticket(ticket_id))
                .await?;
            if let Some(users) = payload.get("users").and_then(Value::as_array) {
                for u in users {
                    if let Some(id) = u.get("id").and_then(Value::as_u64) {
                        users_by_id.insert(id, u.clone());
                    }
                }
            }
            if let Some(comments) = payload.get("comments").and_then(Value::as_array) {
                raw.extend(comments.iter().cloned());
            }
            path = next_page_url(&payload);
            params = None;
        }
        Err(ZendeskError::PaginationLoop(MAX_PAGES))
    }

    /// Search a requester's tickets for customer-history enrichment. Never raises.
    pub async fn fetch_customer_history(
        &self,
        requester_email: &str,
        limit: u32,
    ) -> Vec<TicketSummary> {
        if requester_email.is_empty() {
            return Vec::new();
        }
        let payload = match self
            .get_json(
                "/search.json",
                Some(&[
                    ("query", &format!("requester:{requester_email} type:ticket")),
                    ("sort_by", "updated_at"),
                    ("sort_order", "desc"),
                    ("per_page", &limit.to_string()),
                ]),
                NotFoundKind::Ticket(0),
            )
            .await
        {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        let results = payload
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut out: Vec<TicketSummary> = Vec::new();
        for r in results.iter().take(limit as usize) {
            let summary = (|| -> Option<TicketSummary> {
                Some(TicketSummary {
                    id: r.get("id").and_then(Value::as_u64)?,
                    subject: r
                        .get("subject")
                        .and_then(Value::as_str)
                        .unwrap_or("(no subject)")
                        .to_string(),
                    status: r
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    created_at: parse_iso(r.get("created_at")).ok()?,
                    updated_at: parse_iso(r.get("updated_at")).ok()?,
                })
            })();
            if let Some(s) = summary {
                out.push(s);
            }
        }
        out
    }

    /// Stream-download an attachment. Returns `(bytes_written, sha256_hex)`.
    pub async fn download_attachment(
        &self,
        url: &str,
        dest_path: &Path,
        max_bytes: u64,
    ) -> Result<(u64, String), ZendeskError> {
        let partial = dest_path.with_extension(format!(
            "{}.partial",
            dest_path
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));

        let response_result = self
            .auth(self.client.get(url))
            .send()
            .await
            .map_err(ZendeskError::AttachmentTransport);
        let resp = match response_result {
            Ok(r) => r,
            Err(e) => {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(e);
            }
        };

        let status = resp.status();
        if status == StatusCode::NOT_FOUND {
            return Err(ZendeskError::AttachmentNotFound(url.to_string()));
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ZendeskError::AttachmentAuthFailed);
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            let ra = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
                .to_string();
            return Err(ZendeskError::RateLimited(ra));
        }
        if !status.is_success() {
            return Err(ZendeskError::HttpStatus(
                status.as_u16(),
                "downloading attachment".to_string(),
            ));
        }

        if let Some(cl) = resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
        {
            if cl > max_bytes {
                return Err(ZendeskError::AttachmentTooLargePreflight(cl, max_bytes));
            }
        }

        let mut hasher = Sha256::new();
        let mut bytes_written: u64 = 0;
        let mut file = match tokio::fs::File::create(&partial).await {
            Ok(f) => f,
            Err(e) => {
                return Err(ZendeskError::HttpStatus(0, e.to_string()));
            }
        };
        let mut stream = resp.bytes_stream();
        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(b) => b,
                Err(e) => {
                    let _ = tokio::fs::remove_file(&partial).await;
                    return Err(ZendeskError::AttachmentTransport(e));
                }
            };
            if bytes_written + chunk.len() as u64 > max_bytes {
                drop(file);
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(ZendeskError::AttachmentTooLargeMidStream(max_bytes));
            }
            if let Err(e) = file.write_all(&chunk).await {
                drop(file);
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(ZendeskError::HttpStatus(0, e.to_string()));
            }
            hasher.update(&chunk);
            bytes_written += chunk.len() as u64;
        }
        if let Err(e) = file.flush().await {
            return Err(ZendeskError::HttpStatus(0, e.to_string()));
        }
        drop(file);
        tokio::fs::rename(&partial, dest_path)
            .await
            .map_err(|e| ZendeskError::HttpStatus(0, e.to_string()))?;

        Ok((bytes_written, format!("{:x}", hasher.finalize())))
    }

    async fn get_json(
        &self,
        path: &str,
        params: Option<&[(&str, &str)]>,
        not_found: NotFoundKind,
    ) -> Result<Value, ZendeskError> {
        // Convert &[(&str, &str)] to the dyn form expected by `get_json_dyn`.
        let converted: Option<Vec<(String, String)>> = params.map(|ps| {
            ps.iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect()
        });
        self.get_json_dyn(path, converted.as_deref(), not_found)
            .await
    }

    async fn get_json_dyn(
        &self,
        path_or_url: &str,
        params: Option<&[(String, String)]>,
        not_found: NotFoundKind,
    ) -> Result<Value, ZendeskError> {
        let url = if path_or_url.starts_with("http") {
            path_or_url.to_string()
        } else {
            format!("{}{}", self.base_url, path_or_url)
        };
        let parsed = Url::parse(&url)
            .map_err(|_| ZendeskError::HttpStatus(0, format!("invalid URL: {url}")))?;

        let mut req = self.client.get(parsed);
        if let Some(ps) = params {
            req = req.query(ps);
        }
        let resp = self
            .auth(req)
            .send()
            .await
            .map_err(ZendeskError::Transport)?;

        let status = resp.status();
        if status.is_success() {
            let text = resp.text().await.map_err(ZendeskError::Transport)?;
            return serde_json::from_str(&text).map_err(ZendeskError::NonJson);
        }
        if status == StatusCode::NOT_FOUND {
            return Err(match not_found {
                NotFoundKind::Ticket(id) => ZendeskError::TicketNotFound(id),
                NotFoundKind::View(id) => ZendeskError::ViewNotFound(id),
            });
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ZendeskError::AuthFailed);
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            let ra = resp
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
                .to_string();
            return Err(ZendeskError::RateLimited(ra));
        }
        let text = resp.text().await.unwrap_or_default();
        let snippet: String = text.chars().take(200).collect();
        Err(ZendeskError::HttpStatus(status.as_u16(), snippet))
    }
}

fn index_by_id(value: Option<&Value>) -> std::collections::HashMap<u64, Value> {
    let mut out = std::collections::HashMap::new();
    if let Some(arr) = value.and_then(Value::as_array) {
        for entry in arr {
            if let Some(id) = entry.get("id").and_then(Value::as_u64) {
                out.insert(id, entry.clone());
            }
        }
    }
    out
}

fn next_page_url(payload: &Value) -> Option<String> {
    let has_more = payload
        .get("meta")
        .and_then(|m| m.get("has_more"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let next = payload
        .get("links")
        .and_then(|l| l.get("next"))
        .and_then(Value::as_str);
    if has_more {
        if let Some(n) = next {
            return Some(n.to_string());
        }
    }
    payload
        .get("next_page")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

#[derive(Deserialize)]
struct RawComment {
    plain_body: Option<String>,
    body: Option<String>,
    author_id: Option<u64>,
    created_at: Option<String>,
    public: Option<bool>,
    #[serde(default)]
    attachments: Vec<Value>,
}

fn to_comment(
    raw: &Value,
    users_by_id: &std::collections::HashMap<u64, Value>,
) -> Result<Comment, ZendeskError> {
    let rc: RawComment = serde_json::from_value(raw.clone()).map_err(ZendeskError::NonJson)?;
    let body = rc.plain_body.or(rc.body).unwrap_or_default();
    let created_at = rc
        .created_at
        .as_deref()
        .ok_or_else(|| ZendeskError::NonJson(serde_json::Error::custom("missing created_at")))?;
    let created_at = parse_iso_str(created_at)?;
    Ok(Comment {
        author: resolve_author(rc.author_id, users_by_id),
        body,
        created_at,
        is_public: rc.public.unwrap_or(false),
        attachments: attachments_from_raw(&rc.attachments),
    })
}

fn attachments_from_raw(raw: &[Value]) -> Vec<AttachmentEvidence> {
    let mut out = Vec::new();
    for raw in raw {
        let filename = raw
            .get("file_name")
            .or_else(|| raw.get("filename"))
            .or_else(|| raw.get("name"))
            .and_then(Value::as_str);
        let Some(filename) = filename else {
            continue;
        };
        let content_type = raw
            .get("content_type")
            .and_then(Value::as_str)
            .map(str::to_string);
        let size = raw
            .get("size")
            .or_else(|| raw.get("size_bytes"))
            .and_then(Value::as_u64);
        let content_url = raw
            .get("content_url")
            .and_then(Value::as_str)
            .map(str::to_string);
        out.push(AttachmentEvidence {
            filename: filename.to_string(),
            content_type,
            size_bytes: size,
            content_url,
            ..Default::default()
        });
    }
    out
}

fn resolve_author(
    author_id: Option<u64>,
    users_by_id: &std::collections::HashMap<u64, Value>,
) -> String {
    let Some(id) = author_id else {
        return "user-unknown".into();
    };
    if let Some(user) = users_by_id.get(&id) {
        if let Some(name) = user.get("name").and_then(Value::as_str) {
            if !name.is_empty() {
                return name.to_string();
            }
        }
        if let Some(email) = user.get("email").and_then(Value::as_str) {
            if !email.is_empty() {
                return email.to_string();
            }
        }
    }
    format!("user-{id}")
}

fn parse_iso(value: Option<&Value>) -> Result<DateTime<Utc>, ZendeskError> {
    let s = value
        .and_then(Value::as_str)
        .ok_or_else(|| ZendeskError::NonJson(serde_json::Error::custom("missing ISO field")))?;
    parse_iso_str(s)
}

fn parse_iso_str(s: &str) -> Result<DateTime<Utc>, ZendeskError> {
    // Zendesk emits trailing-Z timestamps. chrono's `parse_from_rfc3339` handles
    // both forms (`Z` and `+00:00`).
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| ZendeskError::NonJson(serde_json::Error::custom(e.to_string())))
}

// A small helper to construct serde_json errors from string messages without
// needing to expose `serde_json::error::Custom`.
trait CustomErr {
    fn custom<M: std::fmt::Display>(msg: M) -> Self;
}
impl CustomErr for serde_json::Error {
    fn custom<M: std::fmt::Display>(msg: M) -> Self {
        // serde_json::Error doesn't expose a public custom constructor, so we
        // wrap it via serde::de::Error.
        <serde_json::Error as serde::de::Error>::custom(msg)
    }
}
