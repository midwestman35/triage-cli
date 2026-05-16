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
    let comments: Vec<Comment> = ticket.comments.to_vec();
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
            timeline.push(attachment_event_from_comment(
                comment,
                &comment.attachments[a_idx],
                a_idx,
            ));
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
    let visibility = if comment.is_public {
        "public"
    } else {
        "internal"
    };
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

fn sort_timeline(events: &mut [TimelineEvent]) {
    events.sort_by_key(|e| e.timestamp.unwrap_or(DateTime::<Utc>::MAX_UTC));
}

#[allow(dead_code)]
fn _file_basename(p: &Path) -> PathBuf {
    p.file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::FileType;
    use std::io::Write;

    fn write_file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
        p
    }

    #[test]
    fn detect_by_extension_wins() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_file_type(&write_file(d.path(), "a.log", b"anything here")),
            FileType::Log
        );
        assert_eq!(
            detect_file_type(&write_file(d.path(), "a.json", b"not really json")),
            FileType::Json
        );
        for ext in ["txt", "text", "md", "csv"] {
            let p = write_file(d.path(), &format!("a.{ext}"), b"plain");
            assert_eq!(detect_file_type(&p), FileType::Text, "ext {ext}");
        }
    }

    #[test]
    fn detect_extension_is_case_insensitive() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_file_type(&write_file(d.path(), "A.LOG", b"x")),
            FileType::Log
        );
    }

    #[test]
    fn detect_json_by_content_sniff() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_file_type(&write_file(d.path(), "noext", b"  {\"k\": 1}\n")),
            FileType::Json
        );
        assert_eq!(
            detect_file_type(&write_file(d.path(), "arr", b"[1, 2, 3]")),
            FileType::Json
        );
    }

    #[test]
    fn detect_brace_prefix_but_invalid_json_is_text() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_file_type(&write_file(d.path(), "bad", b"{ not valid json")),
            FileType::Text
        );
    }

    #[test]
    fn detect_plain_text_without_extension() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_file_type(&write_file(d.path(), "plain", b"hello world")),
            FileType::Text
        );
    }

    #[test]
    fn detect_empty_file_is_unknown() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_file_type(&write_file(d.path(), "empty", b"")),
            FileType::Unknown
        );
    }

    #[test]
    fn detect_binary_with_nul_is_unknown() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_file_type(&write_file(d.path(), "bin.log", b"abc\0def")),
            FileType::Unknown
        );
    }

    #[test]
    fn detect_invalid_utf8_is_unknown() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            detect_file_type(&write_file(d.path(), "bad.txt", &[0xff, 0xfe, 0xfd])),
            FileType::Unknown
        );
    }

    #[test]
    fn detect_missing_path_is_unknown() {
        assert_eq!(
            detect_file_type(Path::new("/no/such/file/here.log")),
            FileType::Unknown
        );
    }
}
