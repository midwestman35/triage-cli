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

use thiserror::Error;

use crate::models::{EvidenceProvenance, Turn, TurnKind};

#[derive(Debug, Error)]
pub enum ChatError {
    #[error("I/O on conversation log: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON encode/decode on conversation log: {0}")]
    Json(#[from] serde_json::Error),
    #[error("conversation file path missing parent: {0}")]
    PathMissingParent(PathBuf),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{EvidenceProvenance, TurnKind};
    use chrono::Utc;
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
}
