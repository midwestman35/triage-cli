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

use crate::models::Turn;

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
}
