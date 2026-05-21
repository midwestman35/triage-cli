//! Pure-function helpers for ticket ID parsing, site lookup, windows, anchors.
//!
//! All datetimes returned are UTC. The only function that performs I/O is
//! `load_site_map`, which reads the on-disk site map JSON.

use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use thiserror::Error;

use crate::models::{
    AnchorSource, Confidence, IncidentWindow, InvestigationHints, SiteEntry, Ticket,
};

static TICKET_URL_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/(?:agent/)?tickets/(\d+)(?:[/?#].*)?$").unwrap());
static RAW_ID_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\d+$").unwrap());
static SUBJECT_BRACKET_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)\[([a-z0-9_]+)\]").unwrap());
static RFC3339_TS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(?::\d{2})?(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})\b")
        .unwrap()
});
static STATION_HINT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:station|console|workstation)\s*[:#-]?\s*([A-Z][A-Za-z0-9_-]{2,40})\b")
        .unwrap()
});
static CALL_ID_HINT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)\bcall[-\s]?id\s*[:#]?\s*([A-Za-z0-9._@-]{6,128})").unwrap());
static COMPONENT_HINT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(kamailio|freeswitch|free.?switch|webrtc|sbc|sip|ali|cad)\b").unwrap()
});

#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("Empty ticket id")]
    EmptyTicketId,
    #[error("Could not parse ticket id from: {0:?}")]
    UnparseableTicketId(String),
    #[error("Site map not found: {0}")]
    SiteMapNotFound(String),
    #[error("Site map is not valid JSON: {0}")]
    InvalidSiteMapJson(#[source] serde_json::Error),
    #[error("Site map root must be a JSON array")]
    SiteMapNotArray,
    #[error("Site map contains invalid entries: {0}")]
    InvalidSiteMapEntries(#[source] serde_json::Error),
    #[error("--site cannot be empty")]
    EmptySiteOverride,
    #[error("--cnc cannot be empty")]
    EmptyCncOverride,
    #[error("CNC override {0} not found in site map; run 'triage-cli build-map' to refresh")]
    CncOverrideNotFound(String),
    #[error("window minutes must be positive, got {0}")]
    NonPositiveWindow(i32),
}

/// The site-lookup strategy that produced a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteStrategy {
    SiteFlag,
    CncFlag,
    OrgMatch,
    SubjectBracket,
    SiteSubstring,
    FriendlySubstring,
    NoMatch,
}

impl SiteStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SiteFlag => "site_flag",
            Self::CncFlag => "cnc_flag",
            Self::OrgMatch => "org_match",
            Self::SubjectBracket => "subject_bracket",
            Self::SiteSubstring => "site_substring",
            Self::FriendlySubstring => "friendly_substring",
            Self::NoMatch => "no_match",
        }
    }
}

/// Parse a Zendesk ticket ID from a raw number or ticket URL.
pub fn parse_ticket_id(value: &str) -> Result<u64, ExtractError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ExtractError::EmptyTicketId);
    }
    if RAW_ID_RE.is_match(trimmed) {
        return trimmed
            .parse::<u64>()
            .map_err(|_| ExtractError::UnparseableTicketId(value.to_string()));
    }
    if let Some(captures) = TICKET_URL_RE.captures(trimmed) {
        return captures[1]
            .parse::<u64>()
            .map_err(|_| ExtractError::UnparseableTicketId(value.to_string()));
    }
    Err(ExtractError::UnparseableTicketId(value.to_string()))
}

/// Load and validate `cnc-map.json`.
pub fn load_site_map(path: &Path) -> Result<Vec<SiteEntry>, ExtractError> {
    if !path.exists() {
        return Err(ExtractError::SiteMapNotFound(path.display().to_string()));
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| ExtractError::InvalidSiteMapJson(serde_json::Error::io(e)))?;
    let json: serde_json::Value =
        serde_json::from_str(&raw).map_err(ExtractError::InvalidSiteMapJson)?;
    let arr = json.as_array().ok_or(ExtractError::SiteMapNotArray)?;
    let entries: Vec<SiteEntry> = serde_json::from_value(serde_json::Value::Array(arr.clone()))
        .map_err(ExtractError::InvalidSiteMapEntries)?;
    Ok(entries)
}

/// Resolve which `SiteEntry` the ticket is about.
///
/// Priority chain (matches Python `lookup_site`):
/// 1. `site_override`: matches an entry's `site_name` (case-insensitive), else synthetic
/// 2. `cnc_override`: exact CNC UUID match (case-insensitive); error if missing
/// 3. `requester_org` exact match against `friendly_name` (case-insensitive)
/// 4. `[bracket_tag]` in ticket subject: normalize `_/__` → `-`, lookup `site_name`
/// 5. Longest `site_name` substring within `subject\n description` (lowercased)
/// 6. Longest `friendly_name` substring within the same haystack
pub fn lookup_site(
    ticket: &Ticket,
    sites: &[SiteEntry],
    cnc_override: Option<&str>,
    site_override: Option<&str>,
) -> Result<(Option<SiteEntry>, SiteStrategy), ExtractError> {
    if let Some(raw) = site_override {
        if raw.trim().is_empty() {
            return Err(ExtractError::EmptySiteOverride);
        }
        let target = raw.to_ascii_lowercase();
        for entry in sites {
            if entry.site_name.to_ascii_lowercase() == target {
                return Ok((Some(entry.clone()), SiteStrategy::SiteFlag));
            }
        }
        return Ok((
            Some(SiteEntry {
                friendly_name: "(manual)".into(),
                site_name: raw.to_string(),
                cnc: String::new(),
            }),
            SiteStrategy::SiteFlag,
        ));
    }

    if let Some(raw) = cnc_override {
        if raw.trim().is_empty() {
            return Err(ExtractError::EmptyCncOverride);
        }
        let target = raw.to_ascii_lowercase();
        for entry in sites {
            if entry.cnc.to_ascii_lowercase() == target {
                return Ok((Some(entry.clone()), SiteStrategy::CncFlag));
            }
        }
        return Err(ExtractError::CncOverrideNotFound(raw.to_string()));
    }

    let org = ticket
        .requester_org
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !org.is_empty() {
        for entry in sites {
            if entry.friendly_name.to_ascii_lowercase() == org {
                return Ok((Some(entry.clone()), SiteStrategy::OrgMatch));
            }
        }
    }

    // Build a case-folded site_name lookup index for the bracket-tag rule.
    let mut site_name_index: std::collections::HashMap<String, &SiteEntry> =
        std::collections::HashMap::new();
    for e in sites {
        if !e.site_name.is_empty() {
            site_name_index
                .entry(e.site_name.to_ascii_lowercase())
                .or_insert(e);
        }
    }
    static MULTI_UNDERSCORE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"_+").unwrap());
    for capture in SUBJECT_BRACKET_RE.captures_iter(&ticket.subject) {
        let raw = &capture[1];
        let normalized = MULTI_UNDERSCORE_RE
            .replace_all(raw, "-")
            .to_ascii_lowercase();
        if let Some(entry) = site_name_index.get(&normalized) {
            return Ok(((*entry).clone().into(), SiteStrategy::SubjectBracket));
        }
    }

    let haystack = format!("{}\n{}", ticket.subject, ticket.description).to_ascii_lowercase();

    let mut best_site: Option<&SiteEntry> = None;
    for entry in sites {
        let sn = entry.site_name.to_ascii_lowercase();
        if !sn.is_empty()
            && haystack.contains(&sn)
            && best_site.is_none_or(|b| entry.site_name.len() > b.site_name.len())
        {
            best_site = Some(entry);
        }
    }
    if let Some(entry) = best_site {
        return Ok((Some(entry.clone()), SiteStrategy::SiteSubstring));
    }

    let mut best_friendly: Option<&SiteEntry> = None;
    for entry in sites {
        let fn_lc = entry.friendly_name.to_ascii_lowercase();
        if !fn_lc.is_empty()
            && haystack.contains(&fn_lc)
            && best_friendly.is_none_or(|b| entry.friendly_name.len() > b.friendly_name.len())
        {
            best_friendly = Some(entry);
        }
    }
    if let Some(entry) = best_friendly {
        return Ok((Some(entry.clone()), SiteStrategy::FriendlySubstring));
    }

    Ok((None, SiteStrategy::NoMatch))
}

/// Build a `(start, end)` window around an anchor (UTC). Errors on non-positive minutes.
pub fn build_window(
    anchor: DateTime<Utc>,
    minutes: i32,
) -> Result<(DateTime<Utc>, DateTime<Utc>), ExtractError> {
    if minutes <= 0 {
        return Err(ExtractError::NonPositiveWindow(minutes));
    }
    let delta = Duration::minutes(minutes as i64);
    Ok((anchor - delta, anchor + delta))
}

/// Pick the anchor timestamp and report which source won.
/// Priority: `at_flag` -> `extracted` -> `ticket.created_at`.
pub fn resolve_anchor(
    ticket: &Ticket,
    at_flag: Option<DateTime<Utc>>,
    extracted: Option<DateTime<Utc>>,
) -> (DateTime<Utc>, AnchorSource) {
    if let Some(at) = at_flag {
        return (at, AnchorSource::Flag);
    }
    if let Some(ex) = extracted {
        return (ex, AnchorSource::Extracted);
    }
    (ticket.created_at, AnchorSource::CreatedAt)
}

/// Resolve the incident window in deterministic Rust code.
///
/// Priority: explicit `--at` flag -> first explicit RFC3339-ish timestamp in
/// ticket text/comments -> Zendesk `created_at` fallback.
pub fn resolve_incident_window(
    ticket: &Ticket,
    at_flag: Option<DateTime<Utc>>,
    window_minutes: i32,
) -> Result<IncidentWindow, ExtractError> {
    let (anchor, source, confidence, explanation) = if let Some(at) = at_flag {
        (
            at,
            AnchorSource::Flag,
            Confidence::High,
            "Incident anchor provided by --at flag.".to_string(),
        )
    } else if let Some(extracted) = extract_reported_timestamp(ticket) {
        (
            extracted,
            AnchorSource::Extracted,
            Confidence::Medium,
            "Found explicit timestamp in ticket text.".to_string(),
        )
    } else {
        (
            ticket.created_at,
            AnchorSource::CreatedAt,
            Confidence::Low,
            "No explicit incident timestamp found; falling back to ticket created_at.".to_string(),
        )
    };
    let (start, end) = build_window(anchor, window_minutes)?;
    Ok(IncidentWindow {
        anchor,
        start,
        end,
        source,
        confidence,
        explanation,
    })
}

fn extract_reported_timestamp(ticket: &Ticket) -> Option<DateTime<Utc>> {
    for body in ticket_texts(ticket) {
        for m in RFC3339_TS_RE.find_iter(body) {
            let mut raw = m.as_str().replace(' ', "T");
            if raw.len() == "YYYY-MM-DDTHH:MMZ".len() {
                raw = raw.replace('Z', ":00Z");
            }
            if let Ok(dt) = DateTime::parse_from_rfc3339(&raw) {
                return Some(dt.with_timezone(&Utc));
            }
        }
    }
    None
}

/// Extract deterministic station, call-id, and component hints from ticket text.
pub fn resolve_investigation_hints(ticket: &Ticket) -> InvestigationHints {
    let mut stations = std::collections::BTreeSet::new();
    let mut call_ids = std::collections::BTreeSet::new();
    let mut components = std::collections::BTreeSet::new();

    for body in ticket_texts(ticket) {
        for cap in STATION_HINT_RE.captures_iter(body) {
            if let Some(raw) = cap.get(1) {
                stations.insert(trim_hint(raw.as_str()));
            }
        }
        for cap in CALL_ID_HINT_RE.captures_iter(body) {
            if let Some(raw) = cap.get(1) {
                call_ids.insert(trim_hint(raw.as_str()));
            }
        }
        for cap in COMPONENT_HINT_RE.captures_iter(body) {
            if let Some(raw) = cap.get(1) {
                components.insert(canonical_component(raw.as_str()));
            }
        }
    }

    InvestigationHints {
        stations: stations.into_iter().filter(|s| !s.is_empty()).collect(),
        call_ids: call_ids.into_iter().filter(|s| !s.is_empty()).collect(),
        components: components.into_iter().filter(|s| !s.is_empty()).collect(),
    }
}

fn ticket_texts(ticket: &Ticket) -> Vec<&str> {
    let mut bodies = Vec::with_capacity(ticket.comments.len() + 2);
    bodies.push(ticket.subject.as_str());
    bodies.push(ticket.description.as_str());
    for comment in &ticket.comments {
        bodies.push(comment.body.as_str());
    }
    bodies
}

fn trim_hint(value: &str) -> String {
    value
        .trim_matches(|c: char| matches!(c, '.' | ',' | ';' | ':' | ')' | ']' | '}'))
        .to_string()
}

fn canonical_component(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("free") && lower.ends_with("switch") {
        "freeswitch".to_string()
    } else {
        lower
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn make_ticket(subject: &str, description: &str, org: Option<&str>) -> Ticket {
        Ticket {
            id: 1,
            subject: subject.into(),
            description: description.into(),
            requester_org: org.map(|s| s.to_string()),
            requester_email: None,
            tags: vec![],
            created_at: Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap(),
            updated_at: None,
            comments: vec![],
        }
    }

    fn make_site(friendly: &str, site: &str, cnc: &str) -> SiteEntry {
        SiteEntry {
            friendly_name: friendly.into(),
            site_name: site.into(),
            cnc: cnc.into(),
        }
    }

    #[test]
    fn parse_id_from_raw() {
        assert_eq!(parse_ticket_id("12345").unwrap(), 12345);
    }

    #[test]
    fn parse_id_from_url() {
        let url = "https://acme.zendesk.com/agent/tickets/98765?x=y";
        assert_eq!(parse_ticket_id(url).unwrap(), 98765);
    }

    #[test]
    fn parse_id_empty_errors() {
        assert!(parse_ticket_id("   ").is_err());
    }

    #[test]
    fn lookup_site_flag_overrides_all() {
        let sites = vec![make_site("Acme", "us-nv-acme", "u1")];
        let t = make_ticket("subj", "desc", Some("Acme"));
        let (entry, strat) = lookup_site(&t, &sites, None, Some("us-nv-other")).unwrap();
        assert!(entry.is_some());
        let entry = entry.unwrap();
        assert_eq!(entry.site_name, "us-nv-other");
        assert_eq!(entry.friendly_name, "(manual)");
        assert_eq!(strat, SiteStrategy::SiteFlag);
    }

    #[test]
    fn lookup_site_org_match() {
        let sites = vec![make_site("Acme PD", "us-nv-acme", "u1")];
        let t = make_ticket("subj", "desc", Some("acme pd"));
        let (entry, strat) = lookup_site(&t, &sites, None, None).unwrap();
        assert_eq!(entry.unwrap().site_name, "us-nv-acme");
        assert_eq!(strat, SiteStrategy::OrgMatch);
    }

    #[test]
    fn build_window_radius() {
        let anchor = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
        let (start, end) = build_window(anchor, 30).unwrap();
        assert_eq!(start, anchor - Duration::minutes(30));
        assert_eq!(end, anchor + Duration::minutes(30));
    }

    #[test]
    fn resolve_anchor_priority_chain() {
        let t = make_ticket("s", "d", None);
        let flag = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let extracted = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap();
        assert_eq!(
            resolve_anchor(&t, Some(flag), Some(extracted)),
            (flag, AnchorSource::Flag)
        );
        assert_eq!(
            resolve_anchor(&t, None, Some(extracted)),
            (extracted, AnchorSource::Extracted)
        );
        let (dt, src) = resolve_anchor(&t, None, None);
        assert_eq!(dt, t.created_at);
        assert_eq!(src, AnchorSource::CreatedAt);
    }

    #[test]
    fn resolve_incident_window_extracts_timestamp_from_ticket_body() {
        let t = make_ticket(
            "Audio dropped",
            "Caller reported audio dropped at 2026-05-16T08:30:00Z before reconnecting.",
            None,
        );

        let window = resolve_incident_window(&t, None, 15).unwrap();

        let anchor = Utc.with_ymd_and_hms(2026, 5, 16, 8, 30, 0).unwrap();
        assert_eq!(window.anchor, anchor);
        assert_eq!(window.start, anchor - Duration::minutes(15));
        assert_eq!(window.end, anchor + Duration::minutes(15));
        assert_eq!(window.source, AnchorSource::Extracted);
        assert_eq!(window.confidence, crate::models::Confidence::Medium);
        assert!(window.explanation.contains("ticket text"));
    }

    #[test]
    fn resolve_incident_window_prefers_flag_and_falls_back_to_created_at() {
        let t = make_ticket("No timestamp", "Customer says it happened earlier.", None);
        let flag = Utc.with_ymd_and_hms(2026, 5, 16, 9, 0, 0).unwrap();

        let flagged = resolve_incident_window(&t, Some(flag), 30).unwrap();
        assert_eq!(flagged.anchor, flag);
        assert_eq!(flagged.source, AnchorSource::Flag);
        assert_eq!(flagged.confidence, crate::models::Confidence::High);

        let fallback = resolve_incident_window(&t, None, 30).unwrap();
        assert_eq!(fallback.anchor, t.created_at);
        assert_eq!(fallback.source, AnchorSource::CreatedAt);
        assert_eq!(fallback.confidence, crate::models::Confidence::Low);
        assert!(fallback.explanation.contains("created_at"));
    }

    #[test]
    fn resolve_investigation_hints_extracts_station_call_id_and_components() {
        let mut t = make_ticket(
            "Audio drop on station Jeffcom-74",
            "Call-ID: abc-123@example.net. Kamailio showed SIP retries near SBC edge.",
            None,
        );
        t.comments.push(crate::models::Comment {
            author: "agent".into(),
            body: "Console Jeffcom-75 also saw WebRTC warnings.".into(),
            created_at: Utc.with_ymd_and_hms(2026, 5, 16, 8, 32, 0).unwrap(),
            is_public: false,
            attachments: vec![],
        });

        let hints = resolve_investigation_hints(&t);

        assert_eq!(hints.stations, vec!["Jeffcom-74", "Jeffcom-75"]);
        assert_eq!(hints.call_ids, vec!["abc-123@example.net"]);
        assert_eq!(hints.components, vec!["kamailio", "sbc", "sip", "webrtc"]);
    }
}
