//! Guided investigation session + evidence helpers. Mirrors Python
//! `triage_cli.investigation` but trimmed to what the Rust pipeline actually
//! uses (deterministic stub assessments live in `pipeline.rs`).

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::models::{
    Comment, FileType, InvestigationEvidence, InvestigationSession, LocalFileEvidence,
    PastedEvidence, Ticket, TimelineEvent,
};

/// Create a guided investigation session from a fetched Zendesk ticket.
pub fn create_session(ticket: Ticket) -> InvestigationSession {
    let comments: Vec<Comment> = ticket.comments.iter().cloned().collect();
    let evidence = InvestigationEvidence {
        ticket_id: ticket.id,
        comments: comments.clone(),
        attachments: Vec::new(),
        local_files: Vec::new(),
        pasted_logs: Vec::new(),
        optional_sources: Vec::new(),
        customer_history: None,
    };

    let mut timeline: Vec<TimelineEvent> = vec![TimelineEvent {
        timestamp: Some(ticket.created_at),
        source: "zendesk".into(),
        kind: "ticket_created".into(),
        message: format!("Ticket created: {}", ticket.subject),
        raw_ref: Some(format!("ticket:{}", ticket.id)),
    }];

    for (index, comment) in comments.iter().enumerate() {
        timeline.push(comment_event(comment, index));
        for (a_idx, _att) in comment.attachments.iter().enumerate() {
            timeline.push(attachment_event_from_comment(comment, &comment.attachments[a_idx], a_idx));
        }
    }
    sort_timeline(&mut timeline);

    InvestigationSession {
        ticket,
        evidence,
        timeline,
        memory_context: None,
    }
}

pub fn add_local_file(
    session: &mut InvestigationSession,
    path: &Path,
) -> Result<LocalFileEvidence, std::io::Error> {
    let stat = fs::metadata(path)?;
    let detected = detect_file_type(path);
    let extracted = read_text_if_supported(path, detected);
    let evidence = LocalFileEvidence {
        path: path.to_path_buf(),
        size_bytes: Some(stat.len()),
        detected_type: Some(detected),
        extracted_text: extracted,
    };
    session.evidence.local_files.push(evidence.clone());
    session.timeline.push(TimelineEvent {
        timestamp: None,
        source: "local_files".into(),
        kind: "local_file".into(),
        message: format!("Local file added: {}", path.display()),
        raw_ref: Some(path.display().to_string()),
    });
    Ok(evidence)
}

pub fn add_pasted_evidence(
    session: &mut InvestigationSession,
    label: &str,
    text: &str,
) -> PastedEvidence {
    let evidence = PastedEvidence {
        label: label.to_string(),
        text: text.to_string(),
    };
    session.evidence.pasted_logs.push(evidence.clone());
    session.timeline.push(TimelineEvent {
        timestamp: None,
        source: "pasted_logs".into(),
        kind: "pasted_log".into(),
        message: format!("Pasted evidence added: {label}"),
        raw_ref: Some(label.to_string()),
    });
    evidence
}

fn comment_event(comment: &Comment, index: usize) -> TimelineEvent {
    let visibility = if comment.is_public { "public" } else { "internal" };
    let first_line = first_line(&comment.body);
    TimelineEvent {
        timestamp: Some(comment.created_at),
        source: "comments".into(),
        kind: "comment".into(),
        message: format!("{visibility} comment from {}: {first_line}", comment.author),
        raw_ref: Some(format!("comment:{index}")),
    }
}

fn attachment_event_from_comment(
    comment: &Comment,
    attachment: &crate::models::AttachmentEvidence,
    index: usize,
) -> TimelineEvent {
    TimelineEvent {
        timestamp: Some(comment.created_at),
        source: "attachments".into(),
        kind: "attachment".into(),
        message: format!("Attachment found: {}", attachment.filename),
        raw_ref: Some(format!("attachment:{index}")),
    }
}

pub fn detect_file_type(path: &Path) -> FileType {
    let Ok(content) = fs::read(path) else {
        return FileType::Unknown;
    };
    if content.contains(&0u8) {
        return FileType::Unknown;
    }
    let Ok(decoded) = std::str::from_utf8(&content) else {
        return FileType::Unknown;
    };
    let suffix = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match suffix.as_deref() {
        Some("log") => return FileType::Log,
        Some("json") => return FileType::Json,
        Some("txt" | "text" | "md" | "csv") => return FileType::Text,
        _ => (),
    }
    let stripped = decoded.trim_start();
    if stripped.starts_with('{') || stripped.starts_with('[') {
        if serde_json::from_str::<serde_json::Value>(decoded).is_ok() {
            return FileType::Json;
        }
        return FileType::Text;
    }
    if !decoded.is_empty() {
        FileType::Text
    } else {
        FileType::Unknown
    }
}

pub fn read_text_if_supported(path: &Path, detected_type: FileType) -> Option<String> {
    if !matches!(
        detected_type,
        FileType::Text | FileType::Log | FileType::Json
    ) {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn first_line(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let line = trimmed.lines().next().unwrap_or("");
    line.chars().take(160).collect()
}

fn sort_timeline(events: &mut Vec<TimelineEvent>) {
    events.sort_by_key(|e| e.timestamp.unwrap_or_else(|| DateTime::<Utc>::MAX_UTC));
}

#[allow(dead_code)]
fn _file_basename(p: &Path) -> PathBuf {
    p.file_name().map(PathBuf::from).unwrap_or_else(|| p.to_path_buf())
}
