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

const ZIP_MAGIC: &[u8] = &[0x50, 0x4B, 0x03, 0x04];

pub fn detect_file_type(path: &Path) -> FileType {
    let Ok(content) = fs::read(path) else {
        return FileType::Unknown;
    };
    if content.starts_with(ZIP_MAGIC) {
        return FileType::Zip;
    }
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
    if detected_type == FileType::Zip {
        return extract_zip_text(path).ok();
    }
    if !matches!(
        detected_type,
        FileType::Text | FileType::Log | FileType::Json
    ) {
        return None;
    }
    fs::read_to_string(path).ok()
}

const ZIP_ENTRY_CAP_BYTES: usize = 256 * 1024;
const USEFUL_ZIP_EXTENSIONS: &[&str] = &["log", "txt", "json", "csv", "xml", "sip"];

pub fn extract_zip_text(path: &Path) -> Result<String, std::io::Error> {
    use std::io::Read;
    let file = fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let mut output = String::new();

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let ext = Path::new(&name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        let Some(ext) = ext else { continue };
        if !USEFUL_ZIP_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }

        // Read up to cap+1 so we can detect overflow; read_to_string silently
        // drops the entry if it isn't valid UTF-8, which is what we want for
        // mixed-content archives (binary entries with a `.log` extension).
        let mut buf = String::new();
        if (&mut entry)
            .take((ZIP_ENTRY_CAP_BYTES + 1) as u64)
            .read_to_string(&mut buf)
            .is_err()
        {
            continue;
        }
        let truncated = buf.len() > ZIP_ENTRY_CAP_BYTES;
        if truncated {
            buf.truncate(ZIP_ENTRY_CAP_BYTES);
        }

        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(&format!("=== {name} ===\n"));
        output.push_str(&buf);
        if truncated {
            output.push_str("\n[truncated]\n");
        }
    }

    if output.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no readable log entries found in zip",
        ));
    }
    Ok(output)
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
    use std::io::Write;
    use tempfile::NamedTempFile;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    fn build_zip(entries: &[(&str, &[u8])]) -> NamedTempFile {
        let tmp = NamedTempFile::new().expect("create temp file");
        let handle = tmp.reopen().expect("reopen for writer");
        let mut zw = ZipWriter::new(handle);
        // Stored (no compression) needs no extra features and keeps tests fast.
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, payload) in entries {
            if name.ends_with('/') {
                zw.add_directory(*name, opts).expect("add directory");
            } else {
                zw.start_file(*name, opts).expect("start file");
                zw.write_all(payload).expect("write entry");
            }
        }
        zw.finish().expect("finish archive");
        tmp
    }

    #[test]
    fn extract_zip_text_returns_log_content_with_entry_header() {
        let zip = build_zip(&[("NewUI/apex.log", b"first log line\nsecond log line\n")]);
        let out = extract_zip_text(zip.path()).expect("extraction succeeds");
        assert!(out.contains("=== NewUI/apex.log ==="));
        assert!(out.contains("first log line"));
        assert!(out.contains("second log line"));
    }

    #[test]
    fn extract_zip_text_skips_directory_entries() {
        let zip = build_zip(&[("logs/", b""), ("logs/app.log", b"hello world")]);
        let out = extract_zip_text(zip.path()).expect("extraction succeeds");
        assert!(out.contains("=== logs/app.log ==="));
        assert!(out.contains("hello world"));
    }

    #[test]
    fn extract_zip_text_filters_unsupported_extensions() {
        let zip = build_zip(&[("ignored.png", b"binary blob")]);
        let err = extract_zip_text(zip.path()).expect_err("png-only archive should fail");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn extract_zip_text_truncates_oversized_entries() {
        let big = "abc\n".repeat(150_000); // ~600 KB > 256 KB cap
        let zip = build_zip(&[("big.log", big.as_bytes())]);
        let out = extract_zip_text(zip.path()).expect("oversized entry extraction succeeds");
        assert!(out.contains("[truncated]"), "expected truncation marker");
        assert!(
            out.len() < big.len(),
            "output should be smaller than source"
        );
    }

    #[test]
    fn extract_zip_text_errors_on_empty_archive() {
        let zip = build_zip(&[]);
        let err = extract_zip_text(zip.path()).expect_err("empty archive should fail");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn extract_zip_text_skips_non_utf8_entries() {
        let zip = build_zip(&[("bin.log", &[0xff, 0xfe, 0xfd])]);
        let err = extract_zip_text(zip.path()).expect_err("binary content should be skipped");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn extract_zip_text_separates_multiple_entries() {
        let zip = build_zip(&[("a.log", b"alpha"), ("b.json", b"{\"k\":1}")]);
        let out = extract_zip_text(zip.path()).expect("extraction succeeds");
        assert!(out.contains("=== a.log ==="));
        assert!(out.contains("=== b.json ==="));
        assert!(out.contains("alpha"));
        assert!(out.contains("\"k\":1"));
    }
}
