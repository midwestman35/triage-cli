//! Interactive investigation chat surface (spec § 5).
//!
//! Owns CONVERSATION.jsonl (source of truth for the conversation log)
//! and the derived CONVERSATION.md renderer. Also owns the per-ticket
//! advisory lock, evidence intake with provenance, and the session
//! manifest + base-snapshot read/write paths.
//!
//! See `docs/superpowers/specs/2026-05-17-interactive-investigation-design.md`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::investigation;
use crate::models::{
    BaseEvidenceManifest, EvidenceProvenance, ExtractionStatus, FileType, SessionManifest, Ticket,
    Turn, TurnKind,
};

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
    // Non-UTF-8 content surfaces as ChatError::Io here, which is intentional:
    // torn-recovery only applies to UTF-8 prefixes of valid JSON. Binary garbage
    // is an I/O-level problem, not a JSONL structural one.
    let content = fs::read_to_string(path)?;
    // Normalize CRLF → LF so that writers emitting Windows line-endings
    // (or any future Windows host) don't leave \r attached to every line.
    // Classic \r-only line endings are not handled — they are vanishingly rare.
    let content = content.replace("\r\n", "\n");
    let ends_with_newline = content.ends_with('\n');
    let lines: Vec<&str> = content.split('\n').collect();
    let last_non_empty_idx = lines.iter().rposition(|l| !l.trim().is_empty());

    let mut turns = Vec::new();
    let mut torn_final_line = false;
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        // A chunk is potentially torn only when it is the last non-empty chunk
        // AND the file does not end with a newline (no terminator written).
        let is_last_non_empty = Some(i) == last_non_empty_idx;
        let is_potentially_torn = is_last_non_empty && !ends_with_newline;
        match serde_json::from_str::<Turn>(line) {
            Ok(turn) => turns.push(turn),
            Err(_) if is_potentially_torn => {
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
        let dst = unique_attachment_path(&dst_dir, &basename, &sha256);
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

fn unique_attachment_path(dst_dir: &Path, basename: &str, sha256: &str) -> PathBuf {
    let initial = dst_dir.join(basename);
    if !initial.exists() {
        return initial;
    }

    let source_name = Path::new(basename);
    let stem = source_name
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("attachment");
    let ext = source_name.extension().and_then(|s| s.to_str());
    let short_hash = &sha256[..sha256.len().min(8)];

    for n in 0.. {
        let suffix = if n == 0 {
            short_hash.to_string()
        } else {
            format!("{short_hash}-{n}")
        };
        let candidate_name = match ext {
            Some(ext) if !ext.is_empty() => format!("{stem}-{suffix}.{ext}"),
            _ => format!("{stem}-{suffix}"),
        };
        let candidate = dst_dir.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!("unbounded suffix loop must return")
}

// Private JSON helpers: extract common serde + atomic_write pattern.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), ChatError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    crate::ticket_folder::atomic_write(path, &bytes).map_err(ChatError::Io)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, ChatError> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Path to the per-ticket session state directory.
pub fn session_dir(ticket_dir: &Path) -> PathBuf {
    ticket_dir.join(".session")
}

/// Path to the session manifest file at `<ticket_dir>/.session/manifest.json`.
pub fn manifest_path(ticket_dir: &Path) -> PathBuf {
    session_dir(ticket_dir).join("manifest.json")
}

/// Path to the durable Ticket snapshot at `<ticket_dir>/.session/base-ticket.json`.
pub fn base_ticket_path(ticket_dir: &Path) -> PathBuf {
    session_dir(ticket_dir).join("base-ticket.json")
}

/// Path to the durable evidence manifest at `<ticket_dir>/.session/base-evidence-manifest.json`.
pub fn base_evidence_path(ticket_dir: &Path) -> PathBuf {
    session_dir(ticket_dir).join("base-evidence-manifest.json")
}

/// Path to the conversation JSONL log at `<ticket_dir>/CONVERSATION.jsonl`.
pub fn conversation_jsonl_path(ticket_dir: &Path) -> PathBuf {
    ticket_dir.join("CONVERSATION.jsonl")
}

/// Path to the derived conversation markdown at `<ticket_dir>/CONVERSATION.md`.
pub fn conversation_md_path(ticket_dir: &Path) -> PathBuf {
    ticket_dir.join("CONVERSATION.md")
}

/// Write the session manifest to `<ticket_dir>/.session/manifest.json`.
pub fn write_session_manifest(ticket_dir: &Path, m: &SessionManifest) -> Result<(), ChatError> {
    write_json(&manifest_path(ticket_dir), m)
}

/// Read the session manifest from `<ticket_dir>/.session/manifest.json`.
pub fn read_session_manifest(ticket_dir: &Path) -> Result<SessionManifest, ChatError> {
    read_json(&manifest_path(ticket_dir))
}

/// Read the session manifest, returning `Ok(None)` if the file is missing.
pub fn read_session_manifest_opt(ticket_dir: &Path) -> Result<Option<SessionManifest>, ChatError> {
    if !manifest_path(ticket_dir).exists() {
        return Ok(None);
    }
    read_json(&manifest_path(ticket_dir)).map(Some)
}

/// Write the durable Ticket snapshot to `<ticket_dir>/.session/base-ticket.json`.
pub fn write_base_ticket(ticket_dir: &Path, t: &Ticket) -> Result<(), ChatError> {
    write_json(&base_ticket_path(ticket_dir), t)
}

/// Read the durable Ticket snapshot from `<ticket_dir>/.session/base-ticket.json`.
pub fn read_base_ticket(ticket_dir: &Path) -> Result<Ticket, ChatError> {
    read_json(&base_ticket_path(ticket_dir))
}

/// Write the durable evidence manifest to `<ticket_dir>/.session/base-evidence-manifest.json`.
pub fn write_base_evidence_manifest(
    ticket_dir: &Path,
    m: &BaseEvidenceManifest,
) -> Result<(), ChatError> {
    write_json(&base_evidence_path(ticket_dir), m)
}

/// Read the durable evidence manifest from `<ticket_dir>/.session/base-evidence-manifest.json`.
pub fn read_base_evidence_manifest(ticket_dir: &Path) -> Result<BaseEvidenceManifest, ChatError> {
    read_json(&base_evidence_path(ticket_dir))
}

// ──────────────────────────────────────────────────────────────────────
//  Chat ticket-context preamble (issues #22, #23)
//
//  The chat surface (`tui::inbox::send_analyst_turn` →
//  `pipeline::followup_turn`) used to pass an empty `system_prompt`, so the
//  default Unleash provider (stateless HTTP — every turn is independent) and
//  the first Codex turn answered with zero knowledge of the ticket or the
//  fork decision. On a Codex session-loss the fresh `codex exec` was equally
//  context-blind. These helpers rebuild a *bounded*, PII-redacted context
//  block from the ticket folder (and, for the session-loss safety net, a
//  short replay of prior turns) that callers seed into `system_prompt`.
// ──────────────────────────────────────────────────────────────────────

/// Total byte cap for the ticket-context preamble after redaction. Bounded
/// so it never dominates a provider request (esp. Unleash, where it is
/// prepended on *every* stateless turn). UTF-8-boundary safe.
///
/// Note on caps: each component (`build_ticket_context_preamble`,
/// `build_conversation_replay`) is individually capped at this value, so the
/// fully-assembled `combined_system_prompt` in `pipeline::followup_turn` may
/// reach up to `COMBINED_SYSTEM_PROMPT_CAP_BYTES` (2× + caller prompt). The
/// pipeline applies a final `truncate_on_boundary` pass using that outer cap.
pub const CONTEXT_PREAMBLE_CAP_BYTES: usize = 8 * 1024;

/// Outer cap applied to the fully-assembled combined system prompt in
/// `pipeline::followup_turn` — covers the preamble + replay + caller prompt
/// all stacking on the session-loss path. Two per-component caps plus
/// a reasonable caller prompt → 20 KiB is a safe ceiling that stays
/// well under POSIX arg-space limits.
pub const COMBINED_SYSTEM_PROMPT_CAP_BYTES: usize = 20 * 1024;

/// Per-source cap applied to STATE.md / FORK_PACKET.md before they are
/// concatenated. Keeps a single oversized file from crowding out the other.
const CONTEXT_SECTION_CAP_BYTES: usize = 4 * 1024;

/// Max base-evidence catalog rows folded into the preamble summary.
const CONTEXT_EVIDENCE_ROWS: usize = 20;

/// Default number of most-recent prior turns replayed on the Codex
/// session-loss safety net (issue #23).
pub const CONVERSATION_REPLAY_TURNS: usize = 8;

/// Per-turn body cap inside a conversation replay block.
const CONVERSATION_REPLAY_PER_TURN_BYTES: usize = 1024;

/// Truncate `s` to at most `cap` bytes on a UTF-8 char boundary, appending
/// a `marker` when truncation occurred. Empty input yields an empty string.
pub(crate) fn truncate_on_boundary(s: &str, cap: usize, marker: &str) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    let mut cut = cap;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = s[..cut].to_string();
    out.push_str(marker);
    out
}

/// Build a bounded, PII-redacted ticket-context preamble from the ticket
/// folder for seeding an LLM chat `system_prompt` (issues #22, #23).
///
/// Sources, in order: `STATE.md`, `FORK_PACKET.md`, and a short
/// base-evidence catalog summary (`<ticket_dir>/.session/base-evidence-manifest.json`).
/// Every source is best-effort: a missing or unreadable file is skipped
/// rather than erroring, because the chat surface must keep working even
/// before a full investigation has been written.
///
/// The assembled text is run through [`crate::redact::redact`] (the PII
/// boundary rule — operational identifiers are preserved; caller PII is
/// scrubbed) and then capped at [`CONTEXT_PREAMBLE_CAP_BYTES`].
///
/// Returns `None` when no usable context could be assembled, so callers
/// can fall back to an empty `system_prompt`.
pub fn build_ticket_context_preamble(ticket_dir: &Path) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();

    // STATE.md — the committed fork decision (YAML frontmatter).
    if let Ok(state) = fs::read_to_string(ticket_dir.join("STATE.md")) {
        let state = state.trim();
        if !state.is_empty() {
            sections.push(format!(
                "## Ticket state (STATE.md)\n{}",
                truncate_on_boundary(state, CONTEXT_SECTION_CAP_BYTES, "\n[truncated]")
            ));
        }
    }

    // FORK_PACKET.md — the committed routing decision + reasoning.
    if let Ok(fp) = fs::read_to_string(ticket_dir.join("FORK_PACKET.md")) {
        let fp = fp.trim();
        if !fp.is_empty() {
            sections.push(format!(
                "## Fork packet (FORK_PACKET.md)\n{}",
                truncate_on_boundary(fp, CONTEXT_SECTION_CAP_BYTES, "\n[truncated]")
            ));
        }
    }

    // Base-evidence catalog summary — id / kind / label rows only (bodies
    // are intentionally excluded; they are large and the provider already
    // gets the analyst's freshly attached evidence on the live turn).
    if let Ok(bem) = read_base_evidence_manifest(ticket_dir) {
        if !bem.evidence.is_empty() {
            let mut block =
                String::from("## Base evidence catalog (from the original investigation)\n");
            for entry in bem.evidence.iter().take(CONTEXT_EVIDENCE_ROWS) {
                block.push_str(&format!(
                    "- {} [{}] {}\n",
                    entry.item.id, entry.item.kind, entry.item.label
                ));
            }
            if bem.evidence.len() > CONTEXT_EVIDENCE_ROWS {
                block.push_str(&format!(
                    "- … and {} more evidence item(s)\n",
                    bem.evidence.len() - CONTEXT_EVIDENCE_ROWS
                ));
            }
            sections.push(block.trim_end().to_string());
        }
    }

    if sections.is_empty() {
        return None;
    }

    let preamble = format!(
        "You are assisting an analyst with a triage ticket. The following \
is the committed context for this ticket; treat it as ground truth and \
answer the analyst's questions with it in mind.\n\n{}",
        sections.join("\n\n")
    );

    // PII boundary: anything heading to a provider goes through redact.
    let (redacted, _counts) = crate::redact::redact(&preamble);
    let capped = truncate_on_boundary(
        &redacted,
        CONTEXT_PREAMBLE_CAP_BYTES,
        "\n\n[context truncated]",
    );
    Some(capped)
}

/// Build a bounded, PII-redacted replay of the most recent prior
/// conversation turns. Used as the Codex session-loss safety net (#23):
/// when `codex exec resume` fails ("no rollout found for thread id"), the
/// provider starts a fresh process with no server-side history, so the
/// model would otherwise answer with amnesia. Seeding this replay into the
/// `system_prompt` reconstructs the recent thread.
///
/// Only `Analyst` and `Codex` turns are replayed (System/Automated turns
/// are bookkeeping). At most `max_turns` of the most-recent turns are
/// included, each body capped at [`CONVERSATION_REPLAY_PER_TURN_BYTES`].
/// The whole block is redacted and returned, or `None` if there is nothing
/// to replay.
pub fn build_conversation_replay(turns: &[Turn], max_turns: usize) -> Option<String> {
    let relevant: Vec<&Turn> = turns
        .iter()
        .filter(|t| matches!(t.turn_kind, TurnKind::Analyst | TurnKind::Codex))
        .collect();
    if relevant.is_empty() || max_turns == 0 {
        return None;
    }
    let start = relevant.len().saturating_sub(max_turns);
    let mut block = String::from(
        "## Prior conversation (replayed — the live session was lost)\n\
Earlier turns in this analyst chat, oldest first:\n",
    );
    for t in &relevant[start..] {
        let speaker = match t.turn_kind {
            TurnKind::Analyst => "analyst",
            TurnKind::Codex => "assistant",
            // unreachable given the filter above, but keep total.
            _ => "other",
        };
        let body = truncate_on_boundary(t.body.trim(), CONVERSATION_REPLAY_PER_TURN_BYTES, " […]");
        block.push_str(&format!("\n### turn {} ({})\n{}\n", t.turn, speaker, body));
    }
    let (redacted, _counts) = crate::redact::redact(&block);
    Some(truncate_on_boundary(
        &redacted,
        CONTEXT_PREAMBLE_CAP_BYTES,
        "\n\n[replay truncated]",
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatStage {
    Ingesting,
    ContextAssembled,
    SessionResumeAttempt,
    ProviderAwait,
    ResponseParsed,
    Saved,
}

pub fn canned_message(stage: ChatStage, rotation_idx: usize) -> &'static str {
    match stage {
        ChatStage::Ingesting => "loading attachments…",
        ChatStage::ContextAssembled => "reading the ticket…",
        ChatStage::SessionResumeAttempt => "resuming session…",
        ChatStage::ProviderAwait => {
            const AWAIT_CYCLE: [&str; 4] = [
                "asking around…",
                "thinking it through…",
                "still working…",
                "the model is taking its time…",
            ];
            AWAIT_CYCLE[rotation_idx % AWAIT_CYCLE.len()]
        }
        ChatStage::ResponseParsed => "writing it up…",
        ChatStage::Saved => "saved",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCloseReason {
    UserQuit,
    EscFromAsk,
    EscFromInflight,
    CtrlC,
    ProviderUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelSource {
    EscKey,
    CtrlC,
    AppExit,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatEvent {
    SessionOpened {
        ticket_id: String,
        ts: chrono::DateTime<chrono::Utc>,
    },
    SessionClosed {
        ts: chrono::DateTime<chrono::Utc>,
        reason: SessionCloseReason,
    },
    KeyCommand {
        ts: chrono::DateTime<chrono::Utc>,
        command: String,
    },
    EvidenceAttached {
        ts: chrono::DateTime<chrono::Utc>,
        provenance: EvidenceProvenance,
    },
    EvidenceRejected {
        ts: chrono::DateTime<chrono::Utc>,
        reason: String,
    },
    AnalystAppended {
        ts: chrono::DateTime<chrono::Utc>,
        turn: u32,
    },
    Phase {
        ts: chrono::DateTime<chrono::Utc>,
        stage: ChatStage,
        elapsed_s: f64,
    },
    ProviderRequest {
        ts: chrono::DateTime<chrono::Utc>,
        provider: String,
        model: String,
        prompt_bytes: usize,
        attachments: usize,
        session_id: Option<String>,
    },
    ProviderResponse {
        ts: chrono::DateTime<chrono::Utc>,
        elapsed_s: f64,
        tokens_in: Option<u32>,
        tokens_out: Option<u32>,
        resumed: bool,
        session_id: Option<String>,
    },
    ProviderError {
        ts: chrono::DateTime<chrono::Utc>,
        #[serde(rename = "error_kind")]
        kind: String,
        message: String,
    },
    TurnPersisted {
        ts: chrono::DateTime<chrono::Utc>,
        codex_turn: u32,
    },
    Cancelled {
        ts: chrono::DateTime<chrono::Utc>,
        by: CancelSource,
    },
}

impl PartialEq for ChatEvent {
    fn eq(&self, other: &Self) -> bool {
        use ChatEvent::*;
        match (self, other) {
            (
                SessionOpened {
                    ticket_id: a_id,
                    ts: a_ts,
                },
                SessionOpened {
                    ticket_id: b_id,
                    ts: b_ts,
                },
            ) => a_id == b_id && a_ts == b_ts,
            (
                SessionClosed {
                    ts: a_ts,
                    reason: a,
                },
                SessionClosed {
                    ts: b_ts,
                    reason: b,
                },
            ) => a_ts == b_ts && a == b,
            (
                KeyCommand {
                    ts: a_ts,
                    command: a,
                },
                KeyCommand {
                    ts: b_ts,
                    command: b,
                },
            ) => a_ts == b_ts && a == b,
            (
                EvidenceAttached {
                    ts: a_ts,
                    provenance: a,
                },
                EvidenceAttached {
                    ts: b_ts,
                    provenance: b,
                },
            ) => a_ts == b_ts && serde_json::to_value(a).ok() == serde_json::to_value(b).ok(),
            (
                EvidenceRejected {
                    ts: a_ts,
                    reason: a,
                },
                EvidenceRejected {
                    ts: b_ts,
                    reason: b,
                },
            ) => a_ts == b_ts && a == b,
            (AnalystAppended { ts: a_ts, turn: a }, AnalystAppended { ts: b_ts, turn: b }) => {
                a_ts == b_ts && a == b
            }
            (
                Phase {
                    ts: a_ts,
                    stage: a_stage,
                    elapsed_s: a_elapsed,
                },
                Phase {
                    ts: b_ts,
                    stage: b_stage,
                    elapsed_s: b_elapsed,
                },
            ) => a_ts == b_ts && a_stage == b_stage && a_elapsed == b_elapsed,
            (
                ProviderRequest {
                    ts: a_ts,
                    provider: a_provider,
                    model: a_model,
                    prompt_bytes: a_prompt_bytes,
                    attachments: a_attachments,
                    session_id: a_session_id,
                },
                ProviderRequest {
                    ts: b_ts,
                    provider: b_provider,
                    model: b_model,
                    prompt_bytes: b_prompt_bytes,
                    attachments: b_attachments,
                    session_id: b_session_id,
                },
            ) => {
                a_ts == b_ts
                    && a_provider == b_provider
                    && a_model == b_model
                    && a_prompt_bytes == b_prompt_bytes
                    && a_attachments == b_attachments
                    && a_session_id == b_session_id
            }
            (
                ProviderResponse {
                    ts: a_ts,
                    elapsed_s: a_elapsed,
                    tokens_in: a_tokens_in,
                    tokens_out: a_tokens_out,
                    resumed: a_resumed,
                    session_id: a_session_id,
                },
                ProviderResponse {
                    ts: b_ts,
                    elapsed_s: b_elapsed,
                    tokens_in: b_tokens_in,
                    tokens_out: b_tokens_out,
                    resumed: b_resumed,
                    session_id: b_session_id,
                },
            ) => {
                a_ts == b_ts
                    && a_elapsed == b_elapsed
                    && a_tokens_in == b_tokens_in
                    && a_tokens_out == b_tokens_out
                    && a_resumed == b_resumed
                    && a_session_id == b_session_id
            }
            (
                ProviderError {
                    ts: a_ts,
                    kind: a_kind,
                    message: a_message,
                },
                ProviderError {
                    ts: b_ts,
                    kind: b_kind,
                    message: b_message,
                },
            ) => a_ts == b_ts && a_kind == b_kind && a_message == b_message,
            (
                TurnPersisted {
                    ts: a_ts,
                    codex_turn: a,
                },
                TurnPersisted {
                    ts: b_ts,
                    codex_turn: b,
                },
            ) => a_ts == b_ts && a == b,
            (Cancelled { ts: a_ts, by: a }, Cancelled { ts: b_ts, by: b }) => {
                a_ts == b_ts && a == b
            }
            _ => false,
        }
    }
}

pub const THROBBER_FRAME_COUNT: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct ChatProgress {
    pub stage: ChatStage,
    pub canned_msg: &'static str,
    pub elapsed_s: f64,
    pub frame_idx: usize,
    pub resumed: Option<bool>,
    pub session_id: Option<String>,
}

pub fn update_progress(prev: Option<ChatProgress>, evt: &ChatEvent) -> Option<ChatProgress> {
    match evt {
        ChatEvent::AnalystAppended { .. } => Some(prev.unwrap_or(ChatProgress {
            stage: ChatStage::Ingesting,
            canned_msg: canned_message(ChatStage::Ingesting, 0),
            elapsed_s: 0.0,
            frame_idx: 0,
            resumed: None,
            session_id: None,
        })),
        ChatEvent::Phase {
            stage, elapsed_s, ..
        } => {
            let base = prev.unwrap_or(ChatProgress {
                stage: *stage,
                canned_msg: canned_message(*stage, 0),
                elapsed_s: *elapsed_s,
                frame_idx: 0,
                resumed: None,
                session_id: None,
            });
            Some(ChatProgress {
                stage: *stage,
                canned_msg: canned_message(*stage, (*elapsed_s / 4.0) as usize),
                elapsed_s: *elapsed_s,
                ..base
            })
        }
        ChatEvent::ProviderRequest { session_id, .. } => prev.map(|p| ChatProgress {
            session_id: session_id.clone().or(p.session_id),
            ..p
        }),
        ChatEvent::ProviderResponse {
            resumed,
            session_id,
            elapsed_s,
            ..
        } => prev.map(|p| ChatProgress {
            resumed: Some(*resumed),
            session_id: session_id.clone().or(p.session_id),
            elapsed_s: *elapsed_s,
            ..p
        }),
        ChatEvent::TurnPersisted { .. }
        | ChatEvent::ProviderError { .. }
        | ChatEvent::Cancelled { .. } => None,
        ChatEvent::SessionOpened { .. }
        | ChatEvent::SessionClosed { .. }
        | ChatEvent::KeyCommand { .. }
        | ChatEvent::EvidenceAttached { .. }
        | ChatEvent::EvidenceRejected { .. } => prev,
    }
}

pub fn advance_progress_tick(prev: ChatProgress, elapsed_s: f64) -> ChatProgress {
    let frame_idx = ((elapsed_s * 12.5) as usize) % THROBBER_FRAME_COUNT;
    let rotation_idx = (elapsed_s / 4.0) as usize;
    ChatProgress {
        elapsed_s,
        frame_idx,
        canned_msg: canned_message(prev.stage, rotation_idx),
        ..prev
    }
}

pub fn chat_events_log_path(ticket_dir: &Path) -> PathBuf {
    session_dir(ticket_dir).join("chat-events.log")
}

pub struct ChatLogger {
    writer: Option<std::io::BufWriter<fs::File>>,
}

impl ChatLogger {
    pub fn open(ticket_dir: &Path) -> Result<Self, ChatError> {
        let sdir = session_dir(ticket_dir);
        fs::create_dir_all(&sdir)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(chat_events_log_path(ticket_dir))?;
        Ok(Self {
            writer: Some(std::io::BufWriter::new(file)),
        })
    }

    pub fn log(&mut self, evt: &ChatEvent) {
        let Some(w) = self.writer.as_mut() else {
            return;
        };
        let loggable = loggable_chat_event(evt);
        if let Ok(line) = serde_json::to_string(&loggable) {
            let _ = writeln!(w, "{line}");
            let _ = w.flush();
        }
    }
}

fn loggable_chat_event(evt: &ChatEvent) -> ChatEvent {
    match evt {
        ChatEvent::EvidenceAttached {
            ts,
            provenance:
                EvidenceProvenance::Paste {
                    label,
                    bytes,
                    sent_to_provider,
                    ..
                },
        } => ChatEvent::EvidenceAttached {
            ts: *ts,
            provenance: EvidenceProvenance::Paste {
                label: label.clone(),
                body: "[omitted from chat-events.log]".into(),
                bytes: *bytes,
                sent_to_provider: *sent_to_provider,
            },
        },
        _ => evt.clone(),
    }
}

pub trait ChatPhaseReporter: Send + Sync {
    fn phase(&self, stage: ChatStage);
}

pub struct MpscPhaseReporter {
    tx: tokio::sync::mpsc::UnboundedSender<ChatEvent>,
    started: Instant,
}

impl MpscPhaseReporter {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<ChatEvent>) -> Self {
        Self {
            tx,
            started: Instant::now(),
        }
    }
}

impl ChatPhaseReporter for MpscPhaseReporter {
    fn phase(&self, stage: ChatStage) {
        let _ = self.tx.send(ChatEvent::Phase {
            ts: chrono::Utc::now(),
            stage,
            elapsed_s: self.started.elapsed().as_secs_f64(),
        });
    }
}

pub(crate) fn simple_glob_match(pattern: &str, text: &str) -> bool {
    fn rec(p: &[u8], t: &[u8]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (None, Some(_)) => false,
            (Some(b'*'), _) => rec(&p[1..], t) || (!t.is_empty() && rec(p, &t[1..])),
            (Some(_), None) => p.iter().all(|&b| b == b'*'),
            (Some(b'?'), Some(_)) => rec(&p[1..], &t[1..]),
            (Some(&a), Some(&b)) if a == b => rec(&p[1..], &t[1..]),
            _ => false,
        }
    }
    rec(pattern.as_bytes(), text.as_bytes())
}

#[derive(Debug)]
pub struct DirCollectResult {
    pub attached: Vec<EvidenceProvenance>,
    pub skipped: Vec<DirSkipped>,
}

#[derive(Debug, PartialEq)]
pub enum DirSkipped {
    SizeCapExceeded { path: PathBuf, bytes: u64 },
    UnsupportedType { path: PathBuf },
    GlobMismatch { path: PathBuf },
    ScanCapReached { path: PathBuf, limit: usize },
}

struct DirCollectCtx<'a> {
    ticket_dir: &'a Path,
    turn_no: u32,
    recursive: bool,
    glob: Option<&'a str>,
    cap_files: usize,
    cap_total_bytes: u64,
    ext_allowlist: &'a [&'a str],
}

struct DirCollectState {
    attached: Vec<EvidenceProvenance>,
    skipped: Vec<DirSkipped>,
    running_bytes: u64,
}

pub fn collect_dir_attachments(
    ticket_dir: &Path,
    turn_no: u32,
    dir: &Path,
    recursive: bool,
    glob: Option<&str>,
    cap_files: usize,
    cap_total_bytes: u64,
) -> Result<DirCollectResult, ChatError> {
    const EXT_ALLOWLIST: &[&str] = &[
        "txt", "log", "md", "json", "csv", "yaml", "yml", "conf", "ini", "rs", "py", "ts", "tsx",
        "js", "jsx",
    ];

    let ctx = DirCollectCtx {
        ticket_dir,
        turn_no,
        recursive,
        glob,
        cap_files,
        cap_total_bytes,
        ext_allowlist: EXT_ALLOWLIST,
    };
    let mut state = DirCollectState {
        attached: Vec::new(),
        skipped: Vec::new(),
        running_bytes: 0,
    };
    collect_dir_entries(&ctx, dir, &mut state)?;

    Ok(DirCollectResult {
        attached: state.attached,
        skipped: state.skipped,
    })
}

fn collect_dir_entries(
    ctx: &DirCollectCtx<'_>,
    dir: &Path,
    state: &mut DirCollectState,
) -> Result<bool, ChatError> {
    let entries = fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        if state.attached.len() >= ctx.cap_files {
            state.skipped.push(DirSkipped::ScanCapReached {
                path: dir.to_path_buf(),
                limit: ctx.cap_files,
            });
            return Ok(true);
        }

        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if ctx.recursive && collect_dir_entries(ctx, &path, state)? {
                return Ok(true);
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        let basename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let matches_filter = if let Some(glob) = ctx.glob {
            simple_glob_match(glob, basename)
        } else {
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            investigation::detect_file_type(&path) != FileType::Unknown
                && ctx.ext_allowlist.iter().any(|allowed| *allowed == ext)
        };
        if !matches_filter {
            state.skipped.push(if ctx.glob.is_some() {
                DirSkipped::GlobMismatch { path }
            } else {
                DirSkipped::UnsupportedType { path }
            });
            continue;
        }

        let bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if state.running_bytes.saturating_add(bytes) > ctx.cap_total_bytes {
            state
                .skipped
                .push(DirSkipped::SizeCapExceeded { path, bytes });
            return Ok(true);
        }

        match attach_file(ctx.ticket_dir, ctx.turn_no, &path) {
            Ok(provenance) => {
                state.running_bytes = state.running_bytes.saturating_add(bytes);
                state.attached.push(provenance);
            }
            Err(_) => state.skipped.push(DirSkipped::UnsupportedType { path }),
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{EvidenceProvenance, TurnKind};
    use chrono::{TimeZone, Utc};
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
        // Simulate a torn write: use write! (not writeln!) so there is no trailing
        // newline — physically realistic, since a killed process never completes the
        // final fsync + newline terminator.
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(f, "{{\"schema\":\"triage-cli/conv").unwrap();
        let out = parse_conversation_jsonl(&path).unwrap();
        assert_eq!(out.turns.len(), 1);
        assert!(out.torn_final_line);
    }

    #[test]
    fn newline_terminated_corrupt_final_record_propagates_error() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CONVERSATION.jsonl");
        append_turn(&path, &sample_analyst_turn(1)).unwrap();
        // Append a newline-terminated but malformed record.
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{not_a_valid_turn}}").unwrap();
        let err = parse_conversation_jsonl(&path);
        assert!(
            matches!(err, Err(ChatError::Json(_))),
            "expected ChatError::Json for newline-terminated corrupt record, got {err:?}"
        );
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
    fn all_blank_lines_returns_empty_parse_outcome() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CONVERSATION.jsonl");
        fs::write(&path, b"\n\n\n").unwrap();
        let out = parse_conversation_jsonl(&path).unwrap();
        assert!(out.turns.is_empty());
        assert!(!out.torn_final_line);
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
    fn attach_file_avoids_overwriting_same_basename_in_one_turn() {
        let dir = tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        let src_a = dir.path().join("a/error.log");
        let src_b = dir.path().join("b/error.log");
        write_text_file(&src_a, "first");
        write_text_file(&src_b, "second");

        let prov_a = attach_file(&ticket_dir, 3, &src_a).unwrap();
        let prov_b = attach_file(&ticket_dir, 3, &src_b).unwrap();

        let (path_a, sha_a) = match prov_a {
            EvidenceProvenance::File {
                copied_path,
                sha256,
                ..
            } => (copied_path, sha256),
            _ => panic!("expected File variant"),
        };
        let (path_b, sha_b) = match prov_b {
            EvidenceProvenance::File {
                copied_path,
                sha256,
                ..
            } => (copied_path, sha256),
            _ => panic!("expected File variant"),
        };

        assert_ne!(path_a, path_b);
        assert_eq!(std::fs::read_to_string(&path_a).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&path_b).unwrap(), "second");
        assert_ne!(sha_a, sha_b);
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

    use crate::models::{BaseEvidenceManifest, SessionManifest};

    #[test]
    fn manifest_round_trip() {
        let dir = tempdir().unwrap();
        let ticket_dir = dir.path().to_path_buf();
        let m = SessionManifest {
            version: 1,
            provider: "codex".into(),
            model: "gpt-5.5".into(),
            created_at: Utc::now(),
            last_resumed_at: None,
            resume_count: 0,
            codex_capture_method: Some("stderr_session_id_line".into()),
        };
        write_session_manifest(&ticket_dir, &m).unwrap();
        let back = read_session_manifest(&ticket_dir).unwrap();
        assert_eq!(back.provider, "codex");
        assert_eq!(back.resume_count, 0);
    }

    #[test]
    fn read_missing_manifest_returns_none() {
        let dir = tempdir().unwrap();
        let r = read_session_manifest_opt(dir.path()).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn base_evidence_manifest_round_trip() {
        let dir = tempdir().unwrap();
        let bem = BaseEvidenceManifest {
            schema: "triage-cli/base-evidence".into(),
            schema_version: 1,
            ticket_id: "44776".into(),
            captured_at: Utc::now(),
            evidence: Vec::new(),
        };
        write_base_evidence_manifest(dir.path(), &bem).unwrap();
        let back = read_base_evidence_manifest(dir.path()).unwrap();
        assert_eq!(back.ticket_id, "44776");
    }

    #[test]
    fn round_trip_base_manifest_preserves_body_field() {
        // The v2 schema's `body: Option<String>` field must survive a JSON
        // write→read round trip. Guards the serde-flatten wiring of
        // BaseEvidenceEntry.
        use crate::models::{BaseEvidenceEntry, EvidenceItem};
        let dir = tempdir().unwrap();
        let bem = BaseEvidenceManifest {
            schema: "triage-cli/base-evidence".into(),
            schema_version: 2,
            ticket_id: "44777".into(),
            captured_at: Utc::now(),
            evidence: vec![
                BaseEvidenceEntry {
                    item: EvidenceItem {
                        id: "E-001".into(),
                        kind: "pasted_note".into(),
                        label: "note".into(),
                        source_time: None,
                        source_path: "pasted:note".into(),
                    },
                    body: Some("ROUNDTRIP_BODY_SENTINEL".into()),
                },
                BaseEvidenceEntry {
                    item: EvidenceItem {
                        id: "E-002".into(),
                        kind: "datadog_log_window".into(),
                        label: "window".into(),
                        source_time: None,
                        source_path: "datadog:log_window".into(),
                    },
                    body: None,
                },
            ],
        };
        write_base_evidence_manifest(dir.path(), &bem).unwrap();
        let back = read_base_evidence_manifest(dir.path()).unwrap();
        assert_eq!(back.evidence.len(), 2);
        assert_eq!(
            back.evidence[0].body.as_deref(),
            Some("ROUNDTRIP_BODY_SENTINEL"),
            "v2 body field was dropped on round trip"
        );
        assert!(
            back.evidence[1].body.is_none(),
            "entry without body deserialized with unexpected value"
        );
        // Ensure the catalog metadata is preserved as flat fields.
        assert_eq!(back.evidence[0].item.id, "E-001");
        assert_eq!(back.evidence[1].item.kind, "datadog_log_window");
    }

    // ── ticket-context preamble (#22, #23) ───────────────────────────

    #[test]
    fn preamble_none_when_folder_empty() {
        let dir = tempdir().unwrap();
        // No STATE.md / FORK_PACKET.md / manifest → nothing to assemble.
        assert!(build_ticket_context_preamble(dir.path()).is_none());
    }

    #[test]
    fn preamble_includes_state_fork_and_evidence() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("STATE.md"),
            "---\nticket_id: 44776\nfork: B\nconfidence: medium\n---\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("FORK_PACKET.md"),
            "# FORK PACKET\n\nRecommendation: Fork B — vendor/IT.\n",
        )
        .unwrap();
        let bem = BaseEvidenceManifest {
            schema: "triage-cli/base-evidence".into(),
            schema_version: 2,
            ticket_id: "44776".into(),
            captured_at: Utc::now(),
            evidence: vec![crate::models::BaseEvidenceEntry {
                item: crate::models::EvidenceItem {
                    id: "E-001".into(),
                    kind: "zendesk_comment".into(),
                    label: "first comment".into(),
                    source_time: None,
                    source_path: "zendesk:comment/1".into(),
                },
                body: Some("a body that must NOT appear in the catalog summary".into()),
            }],
        };
        write_base_evidence_manifest(dir.path(), &bem).unwrap();

        let p = build_ticket_context_preamble(dir.path()).expect("preamble must build");
        assert!(p.contains("STATE.md"), "missing STATE section: {p}");
        assert!(p.contains("fork: B"), "missing state body: {p}");
        assert!(p.contains("FORK_PACKET.md"), "missing fork section: {p}");
        assert!(p.contains("Fork B — vendor/IT"), "missing fork body: {p}");
        assert!(
            p.contains("E-001 [zendesk_comment] first comment"),
            "missing evidence catalog row: {p}"
        );
        // Bodies are intentionally excluded from the catalog summary.
        assert!(
            !p.contains("a body that must NOT appear"),
            "evidence body leaked into preamble: {p}"
        );
    }

    #[test]
    fn preamble_redacts_pii() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("FORK_PACKET.md"),
            "Reporter left a callback at (555) 123-4567 in the notes.\n",
        )
        .unwrap();
        let p = build_ticket_context_preamble(dir.path()).expect("preamble must build");
        assert!(
            !p.contains("123-4567"),
            "raw phone leaked through the preamble: {p}"
        );
        assert!(p.contains("<PHONE>"), "redaction sentinel missing: {p}");
    }

    #[test]
    fn preamble_respects_byte_cap() {
        let dir = tempdir().unwrap();
        // Two oversized files; total well over the cap.
        fs::write(
            dir.path().join("STATE.md"),
            "S".repeat(CONTEXT_PREAMBLE_CAP_BYTES * 2),
        )
        .unwrap();
        fs::write(
            dir.path().join("FORK_PACKET.md"),
            "F".repeat(CONTEXT_PREAMBLE_CAP_BYTES * 2),
        )
        .unwrap();
        let p = build_ticket_context_preamble(dir.path()).expect("preamble must build");
        // Cap + the truncation marker (~22 bytes) — allow a small slack.
        assert!(
            p.len() <= CONTEXT_PREAMBLE_CAP_BYTES + 32,
            "preamble exceeded cap: {} bytes",
            p.len()
        );
        assert!(
            p.ends_with("[context truncated]"),
            "expected truncation marker; tail: {:?}",
            &p[p.len().saturating_sub(40)..]
        );
    }

    #[test]
    fn replay_none_without_analyst_or_codex_turns() {
        // Only a System turn → nothing replayable.
        let mut t = sample_analyst_turn(1);
        t.turn_kind = TurnKind::System;
        assert!(build_conversation_replay(&[t], CONVERSATION_REPLAY_TURNS).is_none());
        assert!(build_conversation_replay(&[], CONVERSATION_REPLAY_TURNS).is_none());
    }

    #[test]
    fn replay_keeps_only_last_n_turns_and_redacts() {
        let mut turns = Vec::new();
        for i in 1..=12 {
            let mut t = sample_analyst_turn(i);
            t.body = format!("analyst turn {i}");
            t.evidence.clear();
            turns.push(t);
        }
        // Inject PII into the newest turn.
        let n = turns.len();
        turns[n - 1].body = "ring me at (555) 123-4567".into();

        let replay = build_conversation_replay(&turns, 8).expect("replay must build");
        // Only the last 8 turns (5..=12) are present.
        assert!(replay.contains("turn 12"), "newest turn missing: {replay}");
        assert!(replay.contains("turn 5"), "8th-from-last missing: {replay}");
        assert!(
            !replay.contains("analyst turn 4"),
            "older turn leaked into replay: {replay}"
        );
        // PII scrubbed.
        assert!(
            !replay.contains("123-4567"),
            "PII leaked into replay: {replay}"
        );
        assert!(replay.contains("<PHONE>"), "redaction missing: {replay}");
    }

    #[test]
    fn replay_caps_each_turn_body() {
        let mut t = sample_analyst_turn(1);
        t.body = "x".repeat(CONVERSATION_REPLAY_PER_TURN_BYTES * 4);
        t.evidence.clear();
        let replay = build_conversation_replay(&[t], 8).expect("replay must build");
        assert!(
            replay.contains(" […]"),
            "per-turn truncation marker missing: {replay}"
        );
    }

    #[test]
    fn canned_message_is_non_empty_for_every_stage() {
        for stage in [
            ChatStage::Ingesting,
            ChatStage::ContextAssembled,
            ChatStage::SessionResumeAttempt,
            ChatStage::ProviderAwait,
            ChatStage::ResponseParsed,
            ChatStage::Saved,
        ] {
            for rot in 0..8 {
                let m = canned_message(stage, rot);
                assert!(!m.is_empty(), "stage {stage:?} rot {rot} returned empty");
            }
        }
    }

    #[test]
    fn canned_message_provider_await_rotates_four_distinct_strings() {
        let mut seen = std::collections::HashSet::new();
        for rot in 0..4 {
            seen.insert(canned_message(ChatStage::ProviderAwait, rot));
        }
        assert_eq!(seen.len(), 4, "expected 4 distinct rotations, got {seen:?}");
        assert_eq!(
            canned_message(ChatStage::ProviderAwait, 0),
            canned_message(ChatStage::ProviderAwait, 4)
        );
    }

    #[test]
    fn canned_message_non_await_stages_ignore_rotation() {
        let m0 = canned_message(ChatStage::Ingesting, 0);
        let m9 = canned_message(ChatStage::Ingesting, 9);
        assert_eq!(m0, m9);
    }

    fn now_ts() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap()
    }

    #[test]
    fn chat_event_session_opened_round_trips() {
        let evt = ChatEvent::SessionOpened {
            ticket_id: "44776".into(),
            ts: now_ts(),
        };
        let s = serde_json::to_string(&evt).unwrap();
        assert!(
            s.contains("\"kind\":\"session_opened\""),
            "tag missing: {s}"
        );
        assert!(s.contains("\"ticket_id\":\"44776\""), "field missing: {s}");
        let back: ChatEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(evt, back);
    }

    #[test]
    fn chat_event_phase_round_trips() {
        let evt = ChatEvent::Phase {
            ts: now_ts(),
            stage: ChatStage::ProviderAwait,
            elapsed_s: 2.3,
        };
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"kind\":\"phase\""));
        assert!(s.contains("\"stage\":\"provider_await\""));
        let back: ChatEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(evt, back);
    }

    #[test]
    fn chat_event_provider_request_round_trips() {
        let evt = ChatEvent::ProviderRequest {
            ts: now_ts(),
            provider: "codex".into(),
            model: "gpt-5.5".into(),
            prompt_bytes: 4096,
            attachments: 3,
            session_id: Some("01HFAKE12345".into()),
        };
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"kind\":\"provider_request\""));
        assert!(s.contains("\"prompt_bytes\":4096"));
        let back: ChatEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(evt, back);
    }

    #[test]
    fn chat_event_cancelled_round_trips() {
        let evt = ChatEvent::Cancelled {
            ts: now_ts(),
            by: CancelSource::EscKey,
        };
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"kind\":\"cancelled\""));
        assert!(s.contains("\"by\":\"esc_key\""));
        let back: ChatEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(evt, back);
    }

    #[test]
    fn chat_event_all_variants_serializable() {
        let provenance = EvidenceProvenance::Paste {
            label: "note".into(),
            body: "body".into(),
            bytes: 4,
            sent_to_provider: true,
        };
        let events = vec![
            ChatEvent::SessionOpened {
                ticket_id: "1".into(),
                ts: now_ts(),
            },
            ChatEvent::SessionClosed {
                ts: now_ts(),
                reason: SessionCloseReason::UserQuit,
            },
            ChatEvent::KeyCommand {
                ts: now_ts(),
                command: "send".into(),
            },
            ChatEvent::EvidenceAttached {
                ts: now_ts(),
                provenance: provenance.clone(),
            },
            ChatEvent::EvidenceRejected {
                ts: now_ts(),
                reason: "too big".into(),
            },
            ChatEvent::AnalystAppended {
                ts: now_ts(),
                turn: 7,
            },
            ChatEvent::Phase {
                ts: now_ts(),
                stage: ChatStage::Ingesting,
                elapsed_s: 0.0,
            },
            ChatEvent::ProviderRequest {
                ts: now_ts(),
                provider: "p".into(),
                model: "m".into(),
                prompt_bytes: 1,
                attachments: 0,
                session_id: None,
            },
            ChatEvent::ProviderResponse {
                ts: now_ts(),
                elapsed_s: 1.0,
                tokens_in: None,
                tokens_out: None,
                resumed: false,
                session_id: None,
            },
            ChatEvent::ProviderError {
                ts: now_ts(),
                kind: "io".into(),
                message: "x".into(),
            },
            ChatEvent::TurnPersisted {
                ts: now_ts(),
                codex_turn: 8,
            },
            ChatEvent::Cancelled {
                ts: now_ts(),
                by: CancelSource::CtrlC,
            },
        ];
        for evt in events {
            let s = serde_json::to_string(&evt).unwrap();
            let back: ChatEvent = serde_json::from_str(&s).unwrap();
            assert_eq!(evt, back, "round-trip failed for: {s}");
        }
    }

    #[test]
    fn chat_event_provider_error_uses_error_kind_payload_field() {
        let evt = ChatEvent::ProviderError {
            ts: now_ts(),
            kind: "io".into(),
            message: "x".into(),
        };
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"kind\":\"provider_error\""));
        // The event discriminator already owns `kind`; serde cannot encode a
        // second payload field with the same name without ambiguous JSON.
        assert!(s.contains("\"error_kind\":\"io\""), "serialized: {s}");
    }

    #[test]
    fn update_progress_canonical_sequence_advances_through_stages() {
        let mut p: Option<ChatProgress> = None;
        p = update_progress(
            p,
            &ChatEvent::AnalystAppended {
                ts: now_ts(),
                turn: 7,
            },
        );
        assert!(p.is_some(), "AnalystAppended should open progress");
        p = update_progress(
            p,
            &ChatEvent::Phase {
                ts: now_ts(),
                stage: ChatStage::Ingesting,
                elapsed_s: 0.1,
            },
        );
        assert_eq!(p.as_ref().unwrap().stage, ChatStage::Ingesting);
        assert!((p.as_ref().unwrap().elapsed_s - 0.1).abs() < 1e-6);
        p = update_progress(
            p,
            &ChatEvent::Phase {
                ts: now_ts(),
                stage: ChatStage::ContextAssembled,
                elapsed_s: 0.4,
            },
        );
        assert_eq!(p.as_ref().unwrap().stage, ChatStage::ContextAssembled);
        p = update_progress(
            p,
            &ChatEvent::ProviderRequest {
                ts: now_ts(),
                provider: "codex".into(),
                model: "gpt-5.5".into(),
                prompt_bytes: 1024,
                attachments: 0,
                session_id: Some("01H".into()),
            },
        );
        assert_eq!(p.as_ref().unwrap().session_id.as_deref(), Some("01H"));
        p = update_progress(
            p,
            &ChatEvent::Phase {
                ts: now_ts(),
                stage: ChatStage::ProviderAwait,
                elapsed_s: 0.5,
            },
        );
        assert_eq!(p.as_ref().unwrap().stage, ChatStage::ProviderAwait);
        p = update_progress(
            p,
            &ChatEvent::ProviderResponse {
                ts: now_ts(),
                elapsed_s: 2.0,
                tokens_in: Some(100),
                tokens_out: Some(50),
                resumed: true,
                session_id: Some("01H".into()),
            },
        );
        assert_eq!(p.as_ref().unwrap().resumed, Some(true));
        p = update_progress(
            p,
            &ChatEvent::Phase {
                ts: now_ts(),
                stage: ChatStage::Saved,
                elapsed_s: 2.05,
            },
        );
        assert_eq!(p.as_ref().unwrap().stage, ChatStage::Saved);
        p = update_progress(
            p,
            &ChatEvent::TurnPersisted {
                ts: now_ts(),
                codex_turn: 8,
            },
        );
        assert!(p.is_none(), "TurnPersisted must clear progress");
    }

    #[test]
    fn update_progress_provider_error_clears_progress() {
        let p = update_progress(
            None,
            &ChatEvent::AnalystAppended {
                ts: now_ts(),
                turn: 1,
            },
        );
        let p = update_progress(
            p,
            &ChatEvent::ProviderError {
                ts: now_ts(),
                kind: "io".into(),
                message: "boom".into(),
            },
        );
        assert!(p.is_none());
    }

    #[test]
    fn update_progress_cancel_clears_progress() {
        let p = update_progress(
            None,
            &ChatEvent::AnalystAppended {
                ts: now_ts(),
                turn: 1,
            },
        );
        let p = update_progress(
            p,
            &ChatEvent::Cancelled {
                ts: now_ts(),
                by: CancelSource::EscKey,
            },
        );
        assert!(p.is_none());
    }

    #[test]
    fn update_progress_ignores_lifecycle_and_input_events() {
        let p0 = update_progress(
            None,
            &ChatEvent::SessionOpened {
                ticket_id: "1".into(),
                ts: now_ts(),
            },
        );
        assert!(p0.is_none());
        let p1 = update_progress(
            None,
            &ChatEvent::KeyCommand {
                ts: now_ts(),
                command: "send".into(),
            },
        );
        assert!(p1.is_none());
    }

    #[test]
    fn advance_progress_tick_derives_frame_idx_from_elapsed() {
        let p = ChatProgress {
            stage: ChatStage::ProviderAwait,
            canned_msg: "asking around...",
            elapsed_s: 0.0,
            frame_idx: 0,
            resumed: None,
            session_id: None,
        };
        let a = advance_progress_tick(p.clone(), 1.2);
        let b = advance_progress_tick(p.clone(), 1.2);
        assert_eq!(a.frame_idx, b.frame_idx);
        let c = advance_progress_tick(p.clone(), 0.0);
        let d = advance_progress_tick(p.clone(), 0.5);
        assert!(c.frame_idx != d.frame_idx || c.elapsed_s != d.elapsed_s);
        assert_eq!(d.elapsed_s, 0.5);
    }

    #[test]
    fn advance_progress_tick_rotates_canned_message_for_provider_await() {
        let mut p = ChatProgress {
            stage: ChatStage::ProviderAwait,
            canned_msg: "",
            elapsed_s: 0.0,
            frame_idx: 0,
            resumed: None,
            session_id: None,
        };
        let messages: Vec<&str> = (0..16)
            .map(|i| {
                p = advance_progress_tick(p.clone(), i as f64 * 4.0);
                p.canned_msg
            })
            .collect();
        let unique: std::collections::HashSet<_> = messages.iter().copied().collect();
        assert_eq!(
            unique.len(),
            4,
            "expected 4 distinct rotations: {messages:?}"
        );
    }

    #[test]
    fn chat_logger_round_trips_six_events() {
        let dir = tempdir().unwrap();
        let ticket_dir = dir.path().to_path_buf();
        let mut logger = ChatLogger::open(&ticket_dir).unwrap();
        let events = vec![
            ChatEvent::SessionOpened {
                ticket_id: "44776".into(),
                ts: now_ts(),
            },
            ChatEvent::KeyCommand {
                ts: now_ts(),
                command: "send".into(),
            },
            ChatEvent::AnalystAppended {
                ts: now_ts(),
                turn: 1,
            },
            ChatEvent::Phase {
                ts: now_ts(),
                stage: ChatStage::ProviderAwait,
                elapsed_s: 0.5,
            },
            ChatEvent::TurnPersisted {
                ts: now_ts(),
                codex_turn: 2,
            },
            ChatEvent::SessionClosed {
                ts: now_ts(),
                reason: SessionCloseReason::UserQuit,
            },
        ];
        for evt in &events {
            logger.log(evt);
        }
        drop(logger);
        let body = std::fs::read_to_string(chat_events_log_path(&ticket_dir)).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), events.len());
        for (line, expected) in lines.iter().zip(events.iter()) {
            let back: ChatEvent = serde_json::from_str(line).unwrap();
            assert_eq!(&back, expected);
        }
    }

    #[test]
    fn chat_logger_appends_across_opens() {
        let dir = tempdir().unwrap();
        let ticket_dir = dir.path().to_path_buf();
        {
            let mut logger = ChatLogger::open(&ticket_dir).unwrap();
            logger.log(&ChatEvent::KeyCommand {
                ts: now_ts(),
                command: "send".into(),
            });
        }
        {
            let mut logger = ChatLogger::open(&ticket_dir).unwrap();
            logger.log(&ChatEvent::KeyCommand {
                ts: now_ts(),
                command: "/quit".into(),
            });
        }
        let body = std::fs::read_to_string(chat_events_log_path(&ticket_dir)).unwrap();
        assert_eq!(
            body.lines().count(),
            2,
            "second open should append, not truncate"
        );
    }

    #[test]
    fn chat_logger_omits_paste_body_content() {
        let dir = tempdir().unwrap();
        let ticket_dir = dir.path().to_path_buf();
        let mut logger = ChatLogger::open(&ticket_dir).unwrap();
        logger.log(&ChatEvent::EvidenceAttached {
            ts: now_ts(),
            provenance: EvidenceProvenance::Paste {
                label: "note".into(),
                body: "SECRET_BODY_SENTINEL".into(),
                bytes: 20,
                sent_to_provider: true,
            },
        });
        drop(logger);

        let body = std::fs::read_to_string(chat_events_log_path(&ticket_dir)).unwrap();
        assert!(
            !body.contains("SECRET_BODY_SENTINEL"),
            "body leaked: {body}"
        );
        assert!(body.contains("[omitted from chat-events.log]"));
    }

    #[test]
    fn chat_events_log_path_is_inside_session_dir() {
        let dir = tempdir().unwrap();
        let p = chat_events_log_path(dir.path());
        assert_eq!(p, session_dir(dir.path()).join("chat-events.log"));
    }

    #[tokio::test]
    async fn mpsc_phase_reporter_emits_phase_events_in_order() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
        let reporter = MpscPhaseReporter::new(tx);
        reporter.phase(ChatStage::ContextAssembled);
        reporter.phase(ChatStage::ProviderAwait);
        reporter.phase(ChatStage::ResponseParsed);
        drop(reporter);
        let mut got: Vec<ChatStage> = Vec::new();
        while let Some(evt) = rx.recv().await {
            match evt {
                ChatEvent::Phase { stage, .. } => got.push(stage),
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(
            got,
            vec![
                ChatStage::ContextAssembled,
                ChatStage::ProviderAwait,
                ChatStage::ResponseParsed
            ]
        );
    }

    #[tokio::test]
    async fn mpsc_phase_reporter_does_not_panic_on_closed_channel() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
        drop(rx);
        let reporter = MpscPhaseReporter::new(tx);
        reporter.phase(ChatStage::Ingesting);
    }

    #[test]
    fn simple_glob_match_exact_no_wildcard() {
        assert!(simple_glob_match("station.log", "station.log"));
        assert!(!simple_glob_match("station.log", "other.log"));
    }

    #[test]
    fn simple_glob_match_star_extension() {
        assert!(simple_glob_match("*.log", "station.log"));
        assert!(simple_glob_match("*.log", "a.log"));
        assert!(!simple_glob_match("*.log", "station.txt"));
    }

    #[test]
    fn simple_glob_match_leading_and_trailing_star() {
        assert!(simple_glob_match("station*", "station.log"));
        assert!(simple_glob_match("*log*", "preflight.log.gz"));
        assert!(!simple_glob_match("station*", "preflight.log"));
    }

    #[test]
    fn simple_glob_match_question_mark_one_char() {
        assert!(simple_glob_match("a?.log", "a1.log"));
        assert!(simple_glob_match("a?.log", "ab.log"));
        assert!(!simple_glob_match("a?.log", "abc.log"));
    }

    fn write_text_file(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn collect_dir_respects_file_cap() {
        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("logs");
        for i in 0..30 {
            write_text_file(&src_dir.join(format!("a{i:03}.log")), "data");
        }
        let ticket_dir = dir.path().join("ticket");
        let r = collect_dir_attachments(&ticket_dir, 1, &src_dir, false, None, 25, 4 * 1024 * 1024)
            .unwrap();
        assert_eq!(r.attached.len(), 25);
        assert_eq!(r.skipped.len(), 1);
        assert!(matches!(
            r.skipped[0],
            DirSkipped::ScanCapReached { limit: 25, .. }
        ));
    }

    #[test]
    fn collect_dir_respects_size_cap() {
        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("logs");
        let payload = "X".repeat(1024 * 1024);
        for i in 0..10 {
            write_text_file(&src_dir.join(format!("a{i}.log")), &payload);
        }
        let ticket_dir = dir.path().join("ticket");
        let r =
            collect_dir_attachments(&ticket_dir, 1, &src_dir, false, None, 100, 3 * 1024 * 1024)
                .unwrap();
        assert_eq!(
            r.attached.len(),
            3,
            "expected 3 files within 3 MiB cap, got {}",
            r.attached.len()
        );
        assert_eq!(r.skipped.len(), 1);
        assert!(r
            .skipped
            .iter()
            .all(|s| matches!(s, DirSkipped::SizeCapExceeded { .. })));
    }

    #[test]
    fn collect_dir_glob_filter() {
        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("logs");
        write_text_file(&src_dir.join("a.log"), "x");
        write_text_file(&src_dir.join("b.log"), "x");
        write_text_file(&src_dir.join("notes.txt"), "x");
        let ticket_dir = dir.path().join("ticket");
        let r =
            collect_dir_attachments(&ticket_dir, 1, &src_dir, false, Some("*.log"), 100, 4 << 20)
                .unwrap();
        assert_eq!(r.attached.len(), 2);
        assert_eq!(r.skipped.len(), 1);
        assert!(matches!(r.skipped[0], DirSkipped::GlobMismatch { .. }));
    }

    #[test]
    fn collect_dir_recursive_vs_single_level() {
        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("logs");
        write_text_file(&src_dir.join("top.log"), "x");
        write_text_file(&src_dir.join("nested/deep.log"), "x");
        let ticket_dir = dir.path().join("ticket");
        let r_flat =
            collect_dir_attachments(&ticket_dir, 1, &src_dir, false, None, 100, 4 << 20).unwrap();
        assert_eq!(r_flat.attached.len(), 1);
        let ticket_dir2 = dir.path().join("ticket2");
        let r_recur =
            collect_dir_attachments(&ticket_dir2, 1, &src_dir, true, None, 100, 4 << 20).unwrap();
        assert_eq!(r_recur.attached.len(), 2);
    }

    #[test]
    fn collect_dir_type_allowlist_filters_unknown_extensions() {
        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("logs");
        write_text_file(&src_dir.join("good.log"), "x");
        write_text_file(&src_dir.join("blob.bin"), "x");
        let ticket_dir = dir.path().join("ticket");
        let r =
            collect_dir_attachments(&ticket_dir, 1, &src_dir, false, None, 100, 4 << 20).unwrap();
        assert_eq!(r.attached.len(), 1, "only good.log should attach");
        assert!(r
            .skipped
            .iter()
            .any(|s| matches!(s, DirSkipped::UnsupportedType { .. })));
    }

    #[test]
    fn collect_dir_rejects_unknown_content_even_with_allowed_extension() {
        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("logs");
        write_text_file(&src_dir.join("good.log"), "x");
        if let Some(parent) = src_dir.join("binary.log").parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(src_dir.join("binary.log"), b"x\0y").unwrap();
        let ticket_dir = dir.path().join("ticket");
        let r =
            collect_dir_attachments(&ticket_dir, 1, &src_dir, false, None, 100, 4 << 20).unwrap();
        assert_eq!(r.attached.len(), 1, "binary.log must not attach");
        assert!(r.skipped.iter().any(
            |s| matches!(s, DirSkipped::UnsupportedType { path } if path.ends_with("binary.log"))
        ));
    }
}
