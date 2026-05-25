use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{EvidenceItem, FileType};

/// One entry in `Tickets/<id>/CONVERSATION.jsonl` (spec § 5.1).
/// Source of truth for the conversation log; `CONVERSATION.md` is derived.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub schema: String,
    pub schema_version: u32,
    pub ticket_id: String,
    pub turn: u32,
    pub turn_kind: TurnKind,
    pub ts: DateTime<Utc>,
    pub body: String,

    // analyst / automated turns
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceProvenance>,

    // codex / unleash turns
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_in: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_out: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed: Option<bool>,

    // system turns
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drove_revision_from_turns: Option<Vec<u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TurnKind {
    Analyst,
    Codex,
    System,
    Automated,
}

/// Provenance for a single evidence item attached to a turn (spec § 5.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum EvidenceProvenance {
    File {
        source_path: PathBuf,
        copied_path: PathBuf,
        basename: String,
        sha256: String,
        bytes: u64,
        detected_type: FileType,
        extraction: ExtractionStatus,
        truncated: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        truncation_note: Option<String>,
        sent_to_provider: bool,
    },
    Paste {
        label: String,
        body: String,
        bytes: u64,
        sent_to_provider: bool,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExtractionStatus {
    Full,
    Truncated,
    BinarySkipped,
}

/// Session provenance stored at `Tickets/<id>/.session/manifest.json`
/// (spec § 5.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionManifest {
    pub version: u32,
    pub provider: String,
    pub model: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_resumed_at: Option<DateTime<Utc>>,
    pub resume_count: u32,
    /// Canonical resumable Codex thread id when using app-server transport.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_thread_id: Option<String>,
    /// Transport recorded at session creation: `app-server` or `exec`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_transport: Option<String>,
    /// Records how the session ID was extracted (one of
    /// `codex_json_output`, `stderr_session_id_line`, `none_replay_only`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_capture_method: Option<String>,
}

/// A single entry in the base evidence snapshot. Wraps `EvidenceItem`
/// (catalog metadata: id, kind, label, source pointer) with an optional
/// `body` carrying the actual content captured at the time of the
/// original investigation. Used only by `BaseEvidenceManifest`.
///
/// `EvidenceItem` itself stays lean — `bundle.evidence_index` continues
/// to be `Vec<EvidenceItem>` (catalog-only) for LLM context. The body
/// snapshot lives only in the persisted manifest.
///
/// The `#[serde(flatten)]` on `item` means the JSON wire format is a
/// single flat object with the `EvidenceItem` fields plus an optional
/// `body` field appended. Old v1 manifests (lacking `body`) deserialize
/// cleanly: `body` defaults to `None` via `#[serde(default)]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BaseEvidenceEntry {
    #[serde(flatten)]
    pub item: EvidenceItem,
    /// Snapshot of the content this evidence carried at the time of the
    /// original investigation. `None` for kinds where the body cannot be
    /// captured (e.g. legacy v1 manifests, or when extraction failed).
    /// For local files, the body is the extracted UTF-8 text capped at
    /// `BODY_SNAPSHOT_CAP_BYTES` (see pipeline.rs); for pasted notes,
    /// it's the full text; for Datadog windows, it's the rendered log
    /// lines; for Zendesk comments, the comment body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

/// Durable evidence snapshot written at the end of the original
/// `investigate` run (spec § 5.4). `/revise` rebuilds from this — never
/// from parsed markdown.
///
/// Schema v2 (ADR-0003) replaces v1's bare `Vec<EvidenceItem>` with
/// `Vec<BaseEvidenceEntry>` so the body content captured at the time of
/// the original investigation is preserved alongside the catalog. Old
/// v1 manifests deserialize cleanly via serde flatten + defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseEvidenceManifest {
    pub schema: String,
    pub schema_version: u32,
    pub ticket_id: String,
    pub captured_at: DateTime<Utc>,
    pub evidence: Vec<BaseEvidenceEntry>,
}

/// Attachment passed to `LlmProvider::followup` (spec § 5.7 — provider
/// trait extension).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Attachment {
    pub copied_path: PathBuf,
    pub basename: String,
    pub detected_type: FileType,
    pub extracted_text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_analyst_round_trip() {
        let turn = Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44776".into(),
            turn: 1,
            turn_kind: TurnKind::Analyst,
            ts: "2026-05-15T14:20:13Z".parse().unwrap(),
            author: Some("enrique".into()),
            body: "hello".into(),
            evidence: vec![EvidenceProvenance::Paste {
                label: "note".into(),
                body: "x".into(),
                bytes: 1,
                sent_to_provider: true,
            }],
            provider: None,
            model: None,
            tokens_in: None,
            tokens_out: None,
            elapsed_s: None,
            session_id: None,
            resumed: None,
            action: None,
            outcome: None,
            drove_revision_from_turns: None,
            diff: None,
        };
        let json = serde_json::to_string(&turn).unwrap();
        let back: Turn = serde_json::from_str(&json).unwrap();
        assert_eq!(turn.body, back.body);
        assert_eq!(turn.turn_kind, TurnKind::Analyst);
        assert_eq!(back.evidence.len(), 1);
    }

    #[test]
    fn session_manifest_round_trip() {
        let m = SessionManifest {
            version: 1,
            provider: "codex".into(),
            model: "gpt-5.5".into(),
            created_at: "2026-05-15T14:21:02Z".parse().unwrap(),
            last_resumed_at: Some("2026-05-17T09:14:54Z".parse().unwrap()),
            resume_count: 1,
            codex_thread_id: Some("thread-abc".into()),
            codex_transport: Some("app-server".into()),
            codex_capture_method: Some("app_server_thread_id".into()),
        };
        let json = serde_json::to_string_pretty(&m).unwrap();
        let back: SessionManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.provider, "codex");
        assert_eq!(back.resume_count, 1);
        assert_eq!(back.codex_thread_id.as_deref(), Some("thread-abc"));
        assert_eq!(back.codex_transport.as_deref(), Some("app-server"));
    }

    #[test]
    fn session_manifest_legacy_json_without_new_fields() {
        let json = r#"{
            "version": 1,
            "provider": "codex",
            "model": "gpt-5.5",
            "created_at": "2026-05-15T14:21:02Z",
            "resume_count": 0,
            "codex_capture_method": "codex_json_output"
        }"#;
        let back: SessionManifest = serde_json::from_str(json).unwrap();
        assert_eq!(back.provider, "codex");
        assert!(back.codex_thread_id.is_none());
        assert!(back.codex_transport.is_none());
        assert_eq!(
            back.codex_capture_method.as_deref(),
            Some("codex_json_output")
        );
    }
}
