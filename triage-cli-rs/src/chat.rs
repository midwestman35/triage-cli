//! Interactive investigation chat surface (spec § 5).
//!
//! Owns CONVERSATION.jsonl (source of truth for the conversation log)
//! and the derived CONVERSATION.md renderer. Also owns the per-ticket
//! advisory lock, evidence intake with provenance, and the session
//! manifest + base-snapshot read/write paths.
//!
//! See `docs/superpowers/specs/2026-05-17-interactive-investigation-design.md`.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs2::FileExt;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::investigation;
use crate::models::{EvidenceProvenance, ExtractionStatus, Turn, TurnKind};

#[derive(Debug, Error)]
pub enum ChatError {
    #[error("I/O on conversation log: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON encode/decode on conversation log: {0}")]
    Json(#[from] serde_json::Error),
    #[error("conversation file path missing parent: {0}")]
    PathMissingParent(PathBuf),
    #[error("lock contention at {}", lock_path.display())]
    LockContention { lock_path: PathBuf },
}

/// Parse `Tickets/<id>/CONVERSATION.jsonl` into the turns it contains.
/// A torn final line (e.g. process killed mid-write) is detectable by
/// JSON parse failure on the last line; the parser skips it and surfaces
/// the count via the returned `ParseOutcome`.
pub fn parse_conversation_jsonl(path: &Path) -> Result<ParseOutcome, ChatError> {
    if !path.exists() {
        return Ok(ParseOutcome::default());
    }
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut turns = Vec::new();
    let mut torn_final_line = false;
    let lines: Vec<String> = reader.lines().collect::<Result<Vec<_>, _>>()?;
    let last_idx = lines.len().saturating_sub(1);
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Turn>(line) {
            Ok(turn) => turns.push(turn),
            Err(_) if i == last_idx => {
                torn_final_line = true;
            }
            Err(e) => return Err(ChatError::Json(e)),
        }
    }
    Ok(ParseOutcome {
        turns,
        torn_final_line,
    })
}

#[derive(Debug, Default)]
pub struct ParseOutcome {
    pub turns: Vec<Turn>,
    pub torn_final_line: bool,
}

/// Append one turn to `CONVERSATION.jsonl`. The caller MUST hold the
/// per-ticket lock (see [`acquire_session_lock`] in Task 5) while
/// calling this — append + fsync is not atomic against concurrent
/// writers.
pub fn append_turn(path: &Path, turn: &Turn) -> Result<(), ChatError> {
    let parent = path
        .parent()
        .ok_or_else(|| ChatError::PathMissingParent(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(turn)?;
    writeln!(file, "{line}")?;
    file.sync_all()?;
    Ok(())
}

/// Render the JSONL turns as the human-readable CONVERSATION.md.
/// Deterministic and idempotent: same input always produces the same
/// byte string. The markdown is for human reading only — no parser
/// in this codebase ever consumes it.
pub fn render_conversation_md(turns: &[Turn], ticket_id: &str) -> String {
    let mut out = String::new();
    out.push_str("<!-- triage-cli conversation v1 -->\n");
    out.push_str(&format!("<!-- ticket_id: {ticket_id} -->\n\n"));
    for t in turns {
        out.push_str(&render_one_turn(t));
        out.push('\n');
    }
    out
}

fn render_one_turn(t: &Turn) -> String {
    let kind = match t.turn_kind {
        TurnKind::Analyst => "analyst",
        TurnKind::Codex => "codex",
        TurnKind::System => "system",
        TurnKind::Automated => "automated",
    };
    let mut header = format!(
        "## turn-{turn:03} {kind} {ts}",
        turn = t.turn,
        kind = kind,
        ts = t.ts.format("%Y-%m-%dT%H:%M:%SZ"),
    );
    if let Some(p) = &t.provider {
        header.push_str(&format!(" provider={p}"));
    }
    if let Some(m) = &t.model {
        header.push_str(&format!(" model={m}"));
    }
    if let (Some(ti), Some(to)) = (t.tokens_in, t.tokens_out) {
        header.push_str(&format!(" tokens={ti}/{to}"));
    }
    if let Some(e) = t.elapsed_s {
        header.push_str(&format!(" elapsed_s={e}"));
    }
    if let Some(r) = t.resumed {
        header.push_str(&format!(" resumed={r}"));
    }
    if let Some(a) = &t.action {
        header.push_str(&format!(" action={a}"));
    }
    if let Some(o) = &t.outcome {
        header.push_str(&format!(" outcome={o}"));
    }
    let mut body = String::new();
    body.push_str(&header);
    body.push('\n');
    if let Some(author) = &t.author {
        body.push_str(&format!("author: {author}\n"));
    }
    if !t.evidence.is_empty() {
        body.push_str("evidence:\n");
        for ev in &t.evidence {
            match ev {
                EvidenceProvenance::File {
                    basename,
                    bytes,
                    sha256,
                    truncated,
                    ..
                } => {
                    body.push_str(&format!(
                        "  - file: {basename} ({bytes} bytes, sha256:{}{})\n",
                        &sha256[..sha256.len().min(8)],
                        if *truncated { ", truncated" } else { "" },
                    ));
                }
                EvidenceProvenance::Paste { label, bytes, .. } => {
                    body.push_str(&format!("  - paste: {label} ({bytes} bytes)\n"));
                }
            }
        }
    }
    body.push_str("---\n");
    body.push_str(&t.body);
    if !t.body.ends_with('\n') {
        body.push('\n');
    }
    body
}

/// Render the markdown and write atomically.
pub fn write_conversation_md(
    path: &Path,
    turns: &[Turn],
    ticket_id: &str,
) -> Result<(), ChatError> {
    let md = render_conversation_md(turns, ticket_id);
    crate::ticket_folder::atomic_write(path, md.as_bytes()).map_err(ChatError::Io)?;
    Ok(())
}

/// RAII guard that releases the per-ticket lock on drop.
pub struct SessionLockGuard {
    _file: fs::File,
}

/// Acquire the advisory file lock at `<session_dir>/lock`. Retries with
/// 50ms sleep backoff up to `budget`. The lock is released automatically
/// when the returned guard is dropped.
pub fn acquire_session_lock(
    session_dir: &Path,
    budget: Duration,
) -> Result<SessionLockGuard, ChatError> {
    fs::create_dir_all(session_dir)?;
    let lock_path = session_dir.join("lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .truncate(false)
        .open(&lock_path)?;
    let deadline = Instant::now() + budget;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(SessionLockGuard { _file: file }),
            Err(_) if Instant::now() >= deadline => {
                return Err(ChatError::LockContention {
                    lock_path: lock_path.clone(),
                });
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

const MAX_RAW_BYTES_SOFT_WARN: u64 = 1 << 20; // 1 MB
const TRUNCATE_TEXT_BYTES: usize = 256 << 10; // 256 KB

/// Attach a file to turn `turn_no` of the conversation under
/// `ticket_dir`. Computes sha256 over the raw bytes, copies the file
/// into `ticket_dir/attachments/turn-NNN/` (unless the source is already
/// under `ticket_dir`), and returns the provenance record.
pub fn attach_file(
    ticket_dir: &Path,
    turn_no: u32,
    source: &Path,
) -> Result<EvidenceProvenance, ChatError> {
    let meta = fs::metadata(source)?;
    let bytes = meta.len();
    let basename = source
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed")
        .to_string();
    let sha256 = sha256_of_file(source)?;
    let detected = investigation::detect_file_type(source);

    // Decide copy destination
    let copied_path = if source.starts_with(ticket_dir) {
        source.to_path_buf()
    } else {
        let dst_dir = ticket_dir
            .join("attachments")
            .join(format!("turn-{turn_no:03}"));
        fs::create_dir_all(&dst_dir)?;
        let dst = dst_dir.join(&basename);
        fs::copy(source, &dst)?;
        dst
    };

    // Decide extraction outcome
    let extracted_text = investigation::read_text_if_supported(&copied_path, detected);
    let (extraction, truncated, truncation_note) = match (&extracted_text, detected) {
        (Some(text), _) if text.len() > TRUNCATE_TEXT_BYTES => (
            ExtractionStatus::Truncated,
            true,
            Some(format!(
                "extracted text truncated to first {} KB",
                TRUNCATE_TEXT_BYTES / 1024
            )),
        ),
        (Some(_), _) => (ExtractionStatus::Full, false, None),
        (None, _) => (ExtractionStatus::BinarySkipped, false, None),
    };

    // Size soft-warn note (the analyst is shown this in the TUI; the
    // provenance row records it as truncation_note when applicable)
    let truncation_note = if bytes > MAX_RAW_BYTES_SOFT_WARN && truncation_note.is_none() {
        Some(format!(
            "raw file size {bytes} bytes exceeds soft-warn threshold {} bytes",
            MAX_RAW_BYTES_SOFT_WARN
        ))
    } else {
        truncation_note
    };

    Ok(EvidenceProvenance::File {
        source_path: source.to_path_buf(),
        copied_path,
        basename,
        sha256,
        bytes,
        detected_type: detected,
        extraction,
        truncated,
        truncation_note,
        sent_to_provider: !matches!(extraction, ExtractionStatus::BinarySkipped),
    })
}

/// Attach a labeled paste to a turn. Returns the provenance record.
pub fn attach_paste(label: &str, body: &str) -> EvidenceProvenance {
    EvidenceProvenance::Paste {
        label: label.to_string(),
        body: body.to_string(),
        bytes: body.len() as u64,
        sent_to_provider: true,
    }
}

fn sha256_of_file(path: &Path) -> Result<String, ChatError> {
    use std::io::Read;
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{EvidenceProvenance, TurnKind};
    use chrono::Utc;
    use std::io::Write as IoWrite;
    use std::time::Duration;
    use tempfile::tempdir;

    fn sample_analyst_turn(turn_no: u32) -> Turn {
        Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44776".into(),
            turn: turn_no,
            turn_kind: TurnKind::Analyst,
            ts: Utc::now(),
            author: Some("enrique".into()),
            body: format!("turn {turn_no} body"),
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
        }
    }

    #[test]
    fn parse_empty_file_returns_no_turns() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CONVERSATION.jsonl");
        let out = parse_conversation_jsonl(&path).unwrap();
        assert!(out.turns.is_empty());
        assert!(!out.torn_final_line);
    }

    #[test]
    fn append_then_parse_round_trips_multiple_turns() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CONVERSATION.jsonl");
        append_turn(&path, &sample_analyst_turn(1)).unwrap();
        append_turn(&path, &sample_analyst_turn(2)).unwrap();
        append_turn(&path, &sample_analyst_turn(3)).unwrap();
        let out = parse_conversation_jsonl(&path).unwrap();
        assert_eq!(out.turns.len(), 3);
        assert_eq!(out.turns[0].turn, 1);
        assert_eq!(out.turns[2].turn, 3);
        assert!(!out.torn_final_line);
    }

    #[test]
    fn torn_final_line_is_recovered() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CONVERSATION.jsonl");
        append_turn(&path, &sample_analyst_turn(1)).unwrap();
        // Simulate a torn write: append a partial JSON line
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{\"schema\":\"triage-cli/conv").unwrap();
        let out = parse_conversation_jsonl(&path).unwrap();
        assert_eq!(out.turns.len(), 1);
        assert!(out.torn_final_line);
    }

    #[test]
    fn non_final_corrupt_line_propagates_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CONVERSATION.jsonl");
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{{not json").unwrap();
        let line = serde_json::to_string(&sample_analyst_turn(2)).unwrap();
        writeln!(f, "{line}").unwrap();
        let err = parse_conversation_jsonl(&path);
        assert!(matches!(err, Err(ChatError::Json(_))));
    }

    #[test]
    fn render_md_is_deterministic_and_idempotent() {
        let turns = vec![sample_analyst_turn(1), sample_analyst_turn(2)];
        let md1 = render_conversation_md(&turns, "44776");
        let md2 = render_conversation_md(&turns, "44776");
        assert_eq!(md1, md2);
        assert!(md1.contains("turn-001 analyst"));
        assert!(md1.contains("turn-002 analyst"));
        assert!(md1.starts_with("<!-- triage-cli conversation v1 -->"));
    }

    #[test]
    fn render_md_includes_codex_turn_metadata() {
        let mut t = sample_analyst_turn(2);
        t.turn_kind = TurnKind::Codex;
        t.author = None;
        t.provider = Some("codex".into());
        t.model = Some("gpt-5.5".into());
        t.tokens_in = Some(4200);
        t.tokens_out = Some(980);
        t.elapsed_s = Some(4.1);
        t.resumed = Some(true);
        t.body = "the codex response body".into();
        t.evidence.clear();
        let md = render_conversation_md(&[t], "44776");
        assert!(md.contains("turn-002 codex"));
        assert!(md.contains("provider=codex"));
        assert!(md.contains("model=gpt-5.5"));
        assert!(md.contains("tokens=4200/980"));
        assert!(md.contains("elapsed_s=4.1"));
        assert!(md.contains("resumed=true"));
        assert!(md.contains("the codex response body"));
    }

    #[test]
    fn write_conversation_md_round_trip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CONVERSATION.md");
        let turns = vec![sample_analyst_turn(1)];
        write_conversation_md(&path, &turns, "44776").unwrap();
        let on_disk = fs::read_to_string(&path).unwrap();
        let expected = render_conversation_md(&turns, "44776");
        assert_eq!(on_disk, expected);
    }

    #[test]
    fn lock_acquired_and_released() {
        let dir = tempdir().unwrap();
        let session_dir = dir.path().to_path_buf();
        {
            let guard = acquire_session_lock(&session_dir, Duration::from_secs(1)).unwrap();
            drop(guard);
        }
        // After drop, lock is releasable — re-acquisition succeeds immediately.
        let guard2 = acquire_session_lock(&session_dir, Duration::from_secs(1)).unwrap();
        drop(guard2);
    }

    #[test]
    fn lock_contention_times_out() {
        let dir = tempdir().unwrap();
        let session_dir = dir.path().to_path_buf();
        let _held = acquire_session_lock(&session_dir, Duration::from_secs(1)).unwrap();
        // Second acquisition with a short budget must fail.
        let r = acquire_session_lock(&session_dir, Duration::from_millis(100));
        assert!(matches!(r, Err(ChatError::LockContention { .. })));
    }

    #[test]
    fn attach_file_computes_sha256_and_copies() {
        let dir = tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        let src = dir.path().join("station.log");
        let mut f = fs::File::create(&src).unwrap();
        f.write_all(b"sample log contents").unwrap();
        let prov = attach_file(&ticket_dir, 3, &src).unwrap();
        match prov {
            EvidenceProvenance::File {
                basename,
                copied_path,
                sha256,
                bytes,
                truncated,
                sent_to_provider,
                ..
            } => {
                assert_eq!(basename, "station.log");
                assert!(copied_path.exists());
                assert_eq!(bytes, 19);
                assert!(!truncated);
                assert!(sent_to_provider);
                // sha256 of "sample log contents"
                assert_eq!(sha256.len(), 64);
            }
            _ => panic!("expected File variant"),
        }
    }

    #[test]
    fn attach_file_skips_copy_if_already_inside_ticket_dir() {
        let dir = tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        fs::create_dir_all(&ticket_dir).unwrap();
        let internal = ticket_dir.join("preflight.log");
        let mut f = fs::File::create(&internal).unwrap();
        f.write_all(b"already inside").unwrap();
        let prov = attach_file(&ticket_dir, 3, &internal).unwrap();
        match prov {
            EvidenceProvenance::File {
                source_path,
                copied_path,
                ..
            } => {
                // Same path — no copy.
                assert_eq!(source_path, copied_path);
            }
            _ => panic!("expected File variant"),
        }
    }

    #[test]
    fn attach_paste_records_label_and_bytes() {
        let prov = attach_paste("customer-note", "rebooted twice during the call");
        match prov {
            EvidenceProvenance::Paste {
                label,
                body,
                bytes,
                sent_to_provider,
            } => {
                assert_eq!(label, "customer-note");
                assert!(body.starts_with("rebooted"));
                assert_eq!(bytes, 30);
                assert!(sent_to_provider);
            }
            _ => panic!("expected Paste variant"),
        }
    }
}
