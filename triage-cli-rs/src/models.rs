//! Data models for the triage-cli pipeline. Mirrors Python `triage_cli.models`.

use std::path::PathBuf;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

pub mod chat;

pub use chat::{
    Attachment, BaseEvidenceEntry, BaseEvidenceManifest, EvidenceProvenance, ExtractionStatus,
    SessionManifest, Turn, TurnKind,
};

/// Render a `DateTime<Utc>` as ISO 8601 with `Z` suffix, no microseconds.
pub fn fmt_ts(dt: &DateTime<Utc>) -> String {
    // chrono's `to_rfc3339_opts(Secs, true)` gives us e.g. "2026-05-12T13:45:00Z".
    dt.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Indent continuation lines so wrapped bullets remain visually attached.
pub fn indent_continuations(s: &str) -> String {
    s.replace('\n', "\n  ")
}

/// Keep the first `head_bytes` and last `tail_bytes` of the UTF-8 representation
/// of `text`; insert a marker in between. Mirrors Python's encode→slice→decode.
pub fn truncate_head_tail(text: &str, head_bytes: usize, tail_bytes: usize) -> String {
    let bytes = text.as_bytes();
    if bytes.len() <= head_bytes + tail_bytes {
        return text.to_string();
    }
    let truncated = bytes.len() - head_bytes - tail_bytes;
    let head = decode_lossy(&bytes[..head_bytes]);
    let tail = decode_lossy(&bytes[bytes.len() - tail_bytes..]);
    format!("{head}\n\n[truncated {truncated} bytes]\n\n{tail}")
}

fn decode_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Where the anchor timestamp on a `TriageBundle` came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorSource {
    Flag,
    Extracted,
    CreatedAt,
}

impl AnchorSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Extracted => "extracted",
            Self::CreatedAt => "created_at",
        }
    }
}

/// Calibrated confidence levels emitted by the LLM. Order is meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Metadata for an attachment discovered on a Zendesk ticket.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AttachmentEvidence {
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default = "default_attachment_source")]
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extracted_text: Option<String>,
    /// The pre-signed download URL is kept in-memory only; never persisted.
    #[serde(default, skip_serializing)]
    pub content_url: Option<String>,
}

fn default_attachment_source() -> String {
    "zendesk_attachment".to_string()
}

/// A single Zendesk ticket comment, public or internal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub is_public: bool,
    #[serde(default)]
    pub attachments: Vec<AttachmentEvidence>,
}

/// A Zendesk ticket with its full chronological comment thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: u64,
    pub subject: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_org: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requester_email: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub comments: Vec<Comment>,
}

/// Evidence read from a local path supplied during guided investigation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalFileEvidence {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected_type: Option<FileType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extracted_text: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileType {
    Text,
    Log,
    Json,
    Zip,
    Unknown,
}

impl FileType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Log => "log",
            Self::Json => "json",
            Self::Zip => "zip",
            Self::Unknown => "unknown",
        }
    }
}

/// User-pasted text evidence captured during guided investigation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PastedEvidence {
    pub label: String,
    pub text: String,
}

/// Brief summary of a Zendesk ticket for customer-history context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketSummary {
    pub id: u64,
    pub subject: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Recent ticket history for the same requester.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomerHistoryEvidence {
    pub requester_email: String,
    #[serde(default)]
    pub tickets: Vec<TicketSummary>,
    #[serde(default = "default_customer_history_source")]
    pub source: String,
    pub limit: u32,
}

fn default_customer_history_source() -> String {
    "zendesk_customer_history".to_string()
}

/// One prior investigation retrieved from the memory layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub ticket_id: String,
    pub customer: String,
    pub subject: String,
    pub symptom: String,
    #[serde(default = "default_resolution")]
    pub resolution: String,
    pub assessment: String,
}

fn default_resolution() -> String {
    "[unknown]".to_string()
}

/// Memory-layer retrieval result injected into the investigation session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryContext {
    #[serde(default)]
    pub entries: Vec<MemoryEntry>,
    #[serde(default)]
    pub query_tokens: Vec<String>,
}

/// A single indexed piece of evidence with a stable `E-NNN` ID.
/// Built by `assign_evidence_ids` from a `TriageBundle` after bundle assembly,
/// before the LLM call. Stored in `TriageBundle::evidence_index`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceItem {
    pub id: String,
    pub kind: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_time: Option<DateTime<Utc>>,
    pub source_path: String,
}

/// All evidence gathered for an investigation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvestigationEvidence {
    pub ticket_id: u64,
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub attachments: Vec<AttachmentEvidence>,
    #[serde(default)]
    pub local_files: Vec<LocalFileEvidence>,
    #[serde(default)]
    pub pasted_logs: Vec<PastedEvidence>,
    #[serde(default)]
    pub optional_sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_history: Option<CustomerHistoryEvidence>,
}

/// A normalized event in the investigation timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
    pub source: String,
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_ref: Option<String>,
}

/// State container for the guided investigation. v1 reframe: the final
/// report is the `StructuredTriageReport` returned synchronously by
/// `investigate_one_structured`, so it is not cached on the session itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvestigationSession {
    pub ticket: Ticket,
    pub evidence: InvestigationEvidence,
    #[serde(default)]
    pub timeline: Vec<TimelineEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_context: Option<MemoryContext>,
}

/// A single Datadog log entry within the triage window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub message: String,
    #[serde(default)]
    pub attributes: serde_json::Map<String, serde_json::Value>,
}

/// One entry in `cnc-map.json` mapping a customer to a Datadog site name + CNC UUID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteEntry {
    pub friendly_name: String,
    pub site_name: String,
    pub cnc: String,
}

/// Inputs to the LLM triage call: ticket, customer context, and log window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageBundle {
    pub ticket: Ticket,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site_entry: Option<SiteEntry>,
    #[serde(default)]
    pub log_lines: Vec<LogLine>,
    #[serde(default)]
    pub log_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_source: Option<AnchorSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_start: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub downloaded_attachments: Vec<AttachmentEvidence>,
    #[serde(default)]
    pub local_files: Vec<LocalFileEvidence>,
    #[serde(default)]
    pub pasted_logs: Vec<PastedEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer_history: Option<CustomerHistoryEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_context: Option<MemoryContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_index: Vec<EvidenceItem>,
}

/// Build a deterministically ordered, zero-padded evidence index from a bundle.
///
/// Sort key: `(kind_order, source_time, source_path)` so the same inputs always
/// produce the same `E-NNN` IDs. Callers should populate `bundle.evidence_index`
/// with the result before passing the bundle to the LLM.
pub fn assign_evidence_ids(bundle: &TriageBundle) -> Vec<EvidenceItem> {
    let mut items: Vec<EvidenceItem> = Vec::new();

    // Zendesk comments (one item per comment, sorted by created_at).
    for c in &bundle.ticket.comments {
        let label = format!("{} — {}", fmt_ts(&c.created_at), c.author);
        items.push(EvidenceItem {
            id: String::new(),
            kind: "zendesk_comment".into(),
            label,
            source_time: Some(c.created_at),
            source_path: format!("comment:{}", fmt_ts(&c.created_at)),
        });
    }

    // Downloaded attachments.
    for a in &bundle.downloaded_attachments {
        items.push(EvidenceItem {
            id: String::new(),
            kind: "attachment".into(),
            label: a.filename.clone(),
            source_time: None,
            source_path: format!("attachment:{}", a.filename),
        });
    }

    // Datadog log window (single item).
    if !bundle.log_lines.is_empty() || bundle.window_start.is_some() {
        let label = match (&bundle.site_entry, &bundle.window_start, &bundle.window_end) {
            (Some(s), Some(start), Some(end)) => {
                format!("{} {} to {}", s.site_name, fmt_ts(start), fmt_ts(end))
            }
            (Some(s), _, _) => format!("{} log window", s.site_name),
            _ => "Datadog log window".into(),
        };
        items.push(EvidenceItem {
            id: String::new(),
            kind: "datadog_log_window".into(),
            label,
            source_time: bundle.window_start,
            source_path: "datadog:log_window".into(),
        });
    }

    // Local files (analyst-supplied).
    for lf in &bundle.local_files {
        let name = lf
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| lf.path.display().to_string());
        items.push(EvidenceItem {
            id: String::new(),
            kind: "local_file".into(),
            label: name.clone(),
            source_time: None,
            source_path: format!("local:{name}"),
        });
    }

    // Pasted evidence.
    for p in &bundle.pasted_logs {
        items.push(EvidenceItem {
            id: String::new(),
            kind: "pasted_note".into(),
            label: p.label.clone(),
            source_time: None,
            source_path: format!("pasted:{}", p.label),
        });
    }

    // Customer ticket history (one item if present).
    if let Some(h) = &bundle.customer_history {
        if !h.tickets.is_empty() {
            items.push(EvidenceItem {
                id: String::new(),
                kind: "customer_history".into(),
                label: format!("{} prior ticket(s)", h.tickets.len()),
                source_time: None,
                source_path: "customer_history".into(),
            });
        }
    }

    // Memory hits (one item per prior investigation).
    if let Some(ctx) = &bundle.memory_context {
        for e in &ctx.entries {
            items.push(EvidenceItem {
                id: String::new(),
                kind: "memory_hit".into(),
                label: format!("{} — {}", e.ticket_id, e.subject),
                source_time: None,
                source_path: format!("memory:{}", e.ticket_id),
            });
        }
    }

    // Sort: kind-order first, then source_time (None sorts last), then source_path.
    fn kind_order(kind: &str) -> u8 {
        match kind {
            "zendesk_comment" => 0,
            "attachment" => 1,
            "datadog_log_window" => 2,
            "local_file" => 3,
            "pasted_note" => 4,
            "customer_history" => 5,
            "memory_hit" => 6,
            _ => 7,
        }
    }

    items.sort_by(|a, b| {
        kind_order(&a.kind)
            .cmp(&kind_order(&b.kind))
            .then(a.source_time.cmp(&b.source_time))
            .then(a.source_path.cmp(&b.source_path))
    });

    // Assign zero-padded IDs.
    for (i, item) in items.iter_mut().enumerate() {
        item.id = format!("E-{:03}", i + 1);
    }

    items
}

//
// ──────────────────────────────────────────────────────────────────────
//   v1 reframe — structured ticket-folder report
//   (see `docs/spec/v1-reframe.md` sections 4 and 6)
// ──────────────────────────────────────────────────────────────────────
//

/// Fork letter committed by the LLM. Mirrors `playbook/fork-rubric.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ForkLetter {
    /// Engineering Jira
    A,
    /// Vendor or Internal IT
    B,
    /// NOC self-resolve
    C,
    /// Cannot fork yet — evidence is missing
    D,
}

impl ForkLetter {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::A => "Engineering Jira",
            Self::B => "Vendor or Internal IT",
            Self::C => "NOC self-resolve",
            Self::D => "Cannot fork yet",
        }
    }
}

/// The LLM's committed routing decision plus its citation of the rubric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkCommitment {
    pub fork_letter: ForkLetter,
    pub confidence: Confidence,
    pub quoted_rubric_row: String,
    pub rubric_class: String,
    pub reasoning: String,
}

/// Outcome of validating a `ForkPacket` against the loaded rubric.
/// `warnings` are soft-warn (logged, accepted); `errors` are hard
/// (the caller should retry once, then stash the raw response).
#[derive(Debug, Clone, Default)]
pub struct ValidationOutcome {
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

impl ValidationOutcome {
    pub fn is_acceptable(&self) -> bool {
        self.errors.is_empty()
    }

    pub fn has_warnings(&self) -> bool {
        !self.warnings.is_empty()
    }
}

/// Related Zendesk / Jira / master tickets, plus an optional cluster key.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RelatedWork {
    #[serde(default)]
    pub zendesk: Vec<u64>,
    #[serde(default)]
    pub jira: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster: Option<String>,
}

/// One row of the handoff checklist. `needed` decides the bool; `reason`
/// is a one-line justification rendered into `FORK_PACKET.md`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HandoffItem {
    pub needed: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// Handoff checklist — what downstream action the analyst owes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HandoffBlock {
    pub engineering_jira_needed: HandoffItem,
    pub vendor_or_it_needed: HandoffItem,
    pub customer_note_needed: HandoffItem,
    pub internal_note_needed: HandoffItem,
}

/// `FORK_PACKET.md` content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkPacket {
    pub commitment: ForkCommitment,
    #[serde(default)]
    pub evidence_summary: Vec<String>,
    #[serde(default)]
    pub missing_evidence: Vec<String>,
    #[serde(default)]
    pub related: RelatedWork,
    pub handoff: HandoffBlock,
}

impl ForkPacket {
    /// Validate against the loaded rubric and the spec's coherence rules
    /// (spec section 6). Returns warnings (soft-warn, accepted) and errors
    /// (the caller should retry once, then stash the raw response on a
    /// second failure).
    pub fn validate(&self, rubric: &crate::playbook::Rubric) -> ValidationOutcome {
        let mut out = ValidationOutcome::default();

        // Soft-warn: rubric-row substring (spec decision 1).
        if !rubric.contains_row(&self.commitment.quoted_rubric_row) {
            out.warnings.push(format!(
                "quoted_rubric_row not found verbatim in rubric v{}: {:?}",
                rubric.version(),
                truncate_for_warn(&self.commitment.quoted_rubric_row, 80),
            ));
        }

        // Hard: D requires non-empty missing_evidence.
        if self.commitment.fork_letter == ForkLetter::D && self.missing_evidence.is_empty() {
            out.errors
                .push("fork_letter is D (cannot fork yet) but missing_evidence is empty".into());
        }

        // Hard: D + high confidence is incoherent.
        if self.commitment.fork_letter == ForkLetter::D
            && self.commitment.confidence == Confidence::High
        {
            out.errors.push(
                "fork_letter is D with confidence=high; a high-confidence \
                 'cannot fork yet' is incoherent"
                    .into(),
            );
        }

        out
    }
}

fn truncate_for_warn(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let head: String = s.chars().take(max_chars).collect();
        format!("{head}…")
    }
}

//
// ──────────────────────────────────────────────────────────────────────
/// Snapshot of the Zendesk facts the engine knew before the LLM ran.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IntakeTicketFacts {
    pub zendesk_id: u64,
    pub url: String,
    pub status: String,
    pub priority: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub requester: String,
    pub organization: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cnc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default)]
    pub affected_stations: Vec<String>,
    #[serde(default)]
    pub affected_agents: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    pub incident_window: String,
    pub reported_symptom: String,
}

/// One row of the INTAKE context-pulls table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextPull {
    pub pull: String,
    pub result: String,
    pub source: String,
}

/// Engine's pre-LLM fork hypothesis. The LLM can override this in the
/// FORK_PACKET; this exists so the analyst can audit how close the
/// pre-LLM guess was to the final commitment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InitialRoute {
    pub hypothesis: String,
    pub justification: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IntakeDecision {
    #[default]
    ReadyForEvidencePreflight,
    KnownIssue,
    NeedsClarification,
    CannotProceed,
}

/// `INTAKE.md` content.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IntakeBlock {
    pub housekeeping_complete: bool,
    pub ticket: IntakeTicketFacts,
    pub one_line_fingerprint: String,
    #[serde(default)]
    pub ticket_summary: Vec<String>,
    #[serde(default)]
    pub context_pulls: Vec<ContextPull>,
    pub initial_route: InitialRoute,
    pub intake_decision: IntakeDecision,
}

/// One row of the EVIDENCE_PREFLIGHT gathered-evidence table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GatheredEvidence {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub evidence_type: String,
    pub source: String,
    pub time_window: String,
    pub summary: String,
}

/// `EVIDENCE_PREFLIGHT.md` content.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PreflightBlock {
    #[serde(default)]
    pub gathered: Vec<GatheredEvidence>,
    #[serde(default)]
    pub decisive_evidence: Vec<String>,
    #[serde(default)]
    pub missing_or_non_decisive: Vec<String>,
}

/// Jira draft for fork A. Lives in `DRAFTS.md`, never posted by the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JiraDraft {
    pub title: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_component: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspected_area: Option<String>,
    #[serde(default)]
    pub repro_steps: Vec<String>,
    /// Jira project key. Carbyne default is "REP".
    pub project: String,
}

/// `DRAFTS.md` content. All sections are CONFIRM-gated by the renderer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DraftsBlock {
    pub customer_reply: String,
    pub internal_zendesk_note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jira_draft: Option<JiraDraft>,
}

/// What the LLM emits as a single JSON document. Drives the five-markdown
/// ticket folder. This is the canonical "triage report" of the v1 reframe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredTriageReport {
    pub intake: IntakeBlock,
    pub evidence_preflight: PreflightBlock,
    pub fork_packet: ForkPacket,
    pub drafts: DraftsBlock,
    /// `rubric_version` the LLM was given. The renderer copies this into
    /// `STATE.md` so inbox can detect mismatches against the shipped rubric.
    pub rubric_version: String,
}

impl StructuredTriageReport {
    pub fn validate(&self, rubric: &crate::playbook::Rubric) -> ValidationOutcome {
        let mut out = self.fork_packet.validate(rubric);
        if self.rubric_version != rubric.version() {
            out.warnings.push(format!(
                "report rubric_version ({}) does not match loaded rubric ({})",
                self.rubric_version,
                rubric.version(),
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playbook::Rubric;

    #[test]
    fn fmt_ts_uses_z_suffix() {
        let dt = DateTime::parse_from_rfc3339("2026-05-12T13:45:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(fmt_ts(&dt), "2026-05-12T13:45:00Z");
    }

    #[test]
    fn indent_keeps_first_line_flush() {
        assert_eq!(indent_continuations("a\nb\nc"), "a\n  b\n  c");
    }

    #[test]
    fn truncate_keeps_head_and_tail() {
        let s: String = "x".repeat(50);
        let out = truncate_head_tail(&s, 4, 4);
        assert!(out.starts_with("xxxx"));
        assert!(out.ends_with("xxxx"));
        assert!(out.contains("[truncated"));
    }

    #[test]
    fn assign_evidence_ids_empty_bundle() {
        use chrono::TimeZone;
        let ticket = Ticket {
            id: 1,
            subject: "test".into(),
            description: "test".into(),
            requester_org: None,
            requester_email: None,
            tags: vec![],
            created_at: Utc.with_ymd_and_hms(2026, 5, 12, 0, 0, 0).unwrap(),
            updated_at: None,
            comments: vec![],
        };
        let bundle = TriageBundle {
            ticket,
            site_entry: None,
            log_lines: vec![],
            log_truncated: false,
            anchor: None,
            anchor_source: None,
            window_start: None,
            window_end: None,
            downloaded_attachments: vec![],
            local_files: vec![],
            pasted_logs: vec![],
            customer_history: None,
            memory_context: None,
            evidence_index: vec![],
        };
        let ids = assign_evidence_ids(&bundle);
        assert!(
            ids.is_empty(),
            "empty bundle should produce no evidence items"
        );
    }

    #[test]
    fn assign_evidence_ids_comments_sorted_chronologically() {
        use chrono::TimeZone;
        let t1 = Utc.with_ymd_and_hms(2026, 5, 12, 13, 30, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 5, 12, 14, 0, 0).unwrap();
        let comment = |author: &str, ts: DateTime<Utc>| Comment {
            author: author.into(),
            body: "body".into(),
            created_at: ts,
            is_public: true,
            attachments: vec![],
        };
        let ticket = Ticket {
            id: 2,
            subject: "test".into(),
            description: "d".into(),
            requester_org: None,
            requester_email: None,
            tags: vec![],
            created_at: t1,
            updated_at: None,
            comments: vec![comment("Bob", t2), comment("Alice", t1)],
        };
        let bundle = TriageBundle {
            ticket,
            site_entry: None,
            log_lines: vec![],
            log_truncated: false,
            anchor: None,
            anchor_source: None,
            window_start: None,
            window_end: None,
            downloaded_attachments: vec![],
            local_files: vec![],
            pasted_logs: vec![],
            customer_history: None,
            memory_context: None,
            evidence_index: vec![],
        };
        let ids = assign_evidence_ids(&bundle);
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].id, "E-001");
        assert_eq!(ids[1].id, "E-002");
        // E-001 should be Alice (earlier timestamp), E-002 Bob.
        assert!(
            ids[0].label.contains("Alice"),
            "E-001 should be Alice; got {:?}",
            ids[0].label
        );
        assert!(
            ids[1].label.contains("Bob"),
            "E-002 should be Bob; got {:?}",
            ids[1].label
        );
    }

    #[test]
    fn assign_evidence_ids_stable_determinism() {
        use chrono::TimeZone;
        let t = Utc.with_ymd_and_hms(2026, 5, 12, 0, 0, 0).unwrap();
        let ticket = Ticket {
            id: 3,
            subject: "test".into(),
            description: "d".into(),
            requester_org: None,
            requester_email: None,
            tags: vec![],
            created_at: t,
            updated_at: None,
            comments: vec![Comment {
                author: "Alice".into(),
                body: "b".into(),
                created_at: t,
                is_public: true,
                attachments: vec![],
            }],
        };
        let bundle = TriageBundle {
            ticket,
            site_entry: None,
            log_lines: vec![],
            log_truncated: false,
            anchor: None,
            anchor_source: None,
            window_start: None,
            window_end: None,
            downloaded_attachments: vec![AttachmentEvidence {
                filename: "station.log".into(),
                ..Default::default()
            }],
            local_files: vec![],
            pasted_logs: vec![],
            customer_history: None,
            memory_context: None,
            evidence_index: vec![],
        };
        let first = assign_evidence_ids(&bundle);
        let second = assign_evidence_ids(&bundle);
        assert_eq!(first, second, "assign_evidence_ids must be deterministic");
        assert_eq!(first[0].kind, "zendesk_comment");
        assert_eq!(first[0].id, "E-001");
        assert_eq!(first[1].kind, "attachment");
        assert_eq!(first[1].id, "E-002");
    }
    fn happy_commitment() -> ForkCommitment {
        ForkCommitment {
            fork_letter: ForkLetter::B,
            confidence: Confidence::Medium,
            quoted_rubric_row: "customer LAN, switch, or SDWAN. Link to site master ticket".into(),
            rubric_class: "Symptom Class 3 — Network error banner / WebSocket disconnect / station drops".into(),
            reasoning: "Multiple consoles showed transient network/registration symptoms within the same minute".into(),
        }
    }

    fn packet_with(commitment: ForkCommitment, missing: Vec<String>) -> ForkPacket {
        ForkPacket {
            commitment,
            evidence_summary: vec!["test evidence".into()],
            missing_evidence: missing,
            related: RelatedWork::default(),
            handoff: HandoffBlock::default(),
        }
    }

    #[test]
    fn fork_letter_serializes_uppercase() {
        let json = serde_json::to_string(&ForkLetter::B).unwrap();
        assert_eq!(json, "\"B\"");
        let parsed: ForkLetter = serde_json::from_str("\"D\"").unwrap();
        assert_eq!(parsed, ForkLetter::D);
    }

    #[test]
    fn fork_letter_lowercase_is_rejected() {
        // Renames are UPPERCASE; "a"/"b" must not deserialize as
        // A/B (catches LLMs emitting mixed case silently).
        assert!(serde_json::from_str::<ForkLetter>("\"a\"").is_err());
    }

    #[test]
    fn validate_happy_path_no_warnings_no_errors() {
        let rubric = Rubric::load().unwrap();
        let packet = packet_with(happy_commitment(), vec![]);
        let outcome = packet.validate(&rubric);
        assert!(outcome.is_acceptable());
        assert!(!outcome.has_warnings(), "warnings: {:?}", outcome.warnings);
    }

    #[test]
    fn validate_warns_on_bogus_rubric_row() {
        let rubric = Rubric::load().unwrap();
        let mut c = happy_commitment();
        c.quoted_rubric_row = "this row does not exist anywhere in the rubric".into();
        let packet = packet_with(c, vec![]);
        let outcome = packet.validate(&rubric);
        assert!(
            outcome.is_acceptable(),
            "soft-warn must not reject; got errors: {:?}",
            outcome.errors,
        );
        assert!(outcome.has_warnings());
        assert!(
            outcome
                .warnings
                .iter()
                .any(|w| w.contains("not found verbatim")),
            "expected rubric-miss warning, got: {:?}",
            outcome.warnings,
        );
    }

    #[test]
    fn validate_rejects_fork_d_with_empty_missing_evidence() {
        let rubric = Rubric::load().unwrap();
        let mut c = happy_commitment();
        c.fork_letter = ForkLetter::D;
        c.confidence = Confidence::Low;
        // Use a rubric row that genuinely belongs to the "cannot fork yet" path.
        c.quoted_rubric_row = "Cannot fork yet".into();
        let packet = packet_with(c, vec![]);
        let outcome = packet.validate(&rubric);
        assert!(!outcome.is_acceptable());
        assert!(
            outcome
                .errors
                .iter()
                .any(|e| e.contains("missing_evidence is empty")),
            "expected D+empty-missing error, got: {:?}",
            outcome.errors,
        );
    }

    #[test]
    fn validate_rejects_fork_d_with_high_confidence() {
        let rubric = Rubric::load().unwrap();
        let mut c = happy_commitment();
        c.fork_letter = ForkLetter::D;
        c.confidence = Confidence::High;
        c.quoted_rubric_row = "Cannot fork yet".into();
        let packet = packet_with(c, vec!["need server-side logs".into()]);
        let outcome = packet.validate(&rubric);
        assert!(!outcome.is_acceptable());
        assert!(
            outcome.errors.iter().any(|e| e.contains("incoherent")),
            "expected D+high-confidence error, got: {:?}",
            outcome.errors,
        );
    }

    #[test]
    fn validate_accepts_fork_d_with_low_confidence_and_missing_evidence() {
        let rubric = Rubric::load().unwrap();
        let mut c = happy_commitment();
        c.fork_letter = ForkLetter::D;
        c.confidence = Confidence::Low;
        c.quoted_rubric_row = "Cannot fork yet".into();
        let packet = packet_with(c, vec!["need server-side logs".into()]);
        let outcome = packet.validate(&rubric);
        assert!(outcome.is_acceptable(), "errors: {:?}", outcome.errors);
    }

    #[test]
    fn structured_report_round_trips_serde() {
        let rubric = Rubric::load().unwrap();
        let report = StructuredTriageReport {
            intake: IntakeBlock {
                housekeeping_complete: true,
                ticket: IntakeTicketFacts {
                    zendesk_id: 44671,
                    url: "https://carbyne.zendesk.com/agent/tickets/44671".into(),
                    status: "open".into(),
                    priority: "".into(),
                    tags: vec!["high".into(), "network_issue".into()],
                    requester: "Brandon Jenkins".into(),
                    organization: "JeffCom".into(),
                    site: Some("us-co-jeffcom-apex".into()),
                    cnc: Some("fcef70f9-b814-45eb-bc99-abfb59877d5c".into()),
                    region: Some("gov-west-1".into()),
                    affected_stations: vec!["Jeffcom-74".into()],
                    affected_agents: vec!["Kyler Cook".into()],
                    call_id: None,
                    incident_window: "2026-05-12 06:30:30-06:31:10 UTC".into(),
                    reported_symptom:
                        "All consoles flickered black then showed Network Error Resolved".into(),
                },
                one_line_fingerprint: "JeffCom / us-co-jeffcom-apex / all-console Network Error"
                    .into(),
                ticket_summary: vec!["Customer reports brief all-console outage".into()],
                context_pulls: vec![ContextPull {
                    pull: "Last related Zendesk tickets".into(),
                    result: "Found 43874 (similar symptom)".into(),
                    source: "Zendesk search_tickets".into(),
                }],
                initial_route: InitialRoute {
                    hypothesis: "Fork B (Vendor/IT)".into(),
                    justification: "Multi-console symptom suggests site-network instability".into(),
                },
                intake_decision: IntakeDecision::ReadyForEvidencePreflight,
            },
            evidence_preflight: PreflightBlock::default(),
            fork_packet: packet_with(happy_commitment(), vec![]),
            drafts: DraftsBlock::default(),
            rubric_version: rubric.version().to_string(),
        };

        let json = serde_json::to_string(&report).expect("serialize");
        let back: StructuredTriageReport = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.intake.ticket.zendesk_id, 44671);
        assert_eq!(back.fork_packet.commitment.fork_letter, ForkLetter::B);
        assert_eq!(back.rubric_version, rubric.version());
    }

    #[test]
    fn structured_report_warns_on_version_drift() {
        let rubric = Rubric::load().unwrap();
        let report = StructuredTriageReport {
            intake: IntakeBlock::default(),
            evidence_preflight: PreflightBlock::default(),
            fork_packet: packet_with(happy_commitment(), vec![]),
            drafts: DraftsBlock::default(),
            rubric_version: "1999-01-01".into(),
        };
        let outcome = report.validate(&rubric);
        assert!(outcome.is_acceptable());
        assert!(
            outcome
                .warnings
                .iter()
                .any(|w| w.contains("rubric_version")),
            "expected version-drift warning, got: {:?}",
            outcome.warnings,
        );
    }

    #[test]
    fn truncate_for_warn_keeps_short_strings_intact() {
        assert_eq!(truncate_for_warn("hello", 80), "hello");
    }

    #[test]
    fn truncate_for_warn_marks_truncation() {
        let s = "x".repeat(200);
        let out = truncate_for_warn(&s, 10);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 11); // 10 + the ellipsis
    }
}
