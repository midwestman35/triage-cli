//! Five-markdown ticket folder writer (spec `docs/spec/v1-reframe.md` § 4).
//!
//! Given a `StructuredTriageReport`, this module writes:
//!
//!   Tickets/<id>/INTAKE.md
//!   Tickets/<id>/EVIDENCE_PREFLIGHT.md
//!   Tickets/<id>/FORK_PACKET.md
//!   Tickets/<id>/DRAFTS.md
//!   Tickets/<id>/STATE.md
//!
//! Writes are atomic (tempfile + rename). The folder is created if missing.
//! Soft-lock semantics (Anchor C, spec § 7) are handled by task #5; this
//! module only writes — the caller decides whether overwriting is OK.

use std::path::{Path, PathBuf};

use chrono::Utc;
use thiserror::Error;

use crate::models::{
    fmt_ts, ContextPull, DraftsBlock, ForkLetter, ForkPacket, GatheredEvidence, HandoffItem,
    IntakeBlock, IntakeDecision, JiraDraft, PreflightBlock, StructuredTriageReport,
};

pub const TICKETS_ROOT_ENV: &str = "TRIAGE_TICKETS_ROOT";
pub const DEFAULT_TICKETS_ROOT: &str = "./Tickets";

#[derive(Debug, Error)]
pub enum TicketFolderError {
    #[error("i/o while writing ticket folder: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not serialize STATE.md value: {0}")]
    Serialize(String),
    /// Soft-lock tripwire: an existing `STATE.md` claims a different owner.
    /// The caller can inspect `summary` to render a per-field diff, or open
    /// the full diff using `state_path` (existing) vs. `new_state_content`.
    #[error("STATE.md soft-lock conflict: owned by {existing_owner}, current is {current_owner}")]
    SoftLockConflict {
        existing_owner: String,
        current_owner: String,
        summary: Vec<(String, String, String)>,
        state_path: PathBuf,
        new_state_content: String,
    },
}

/// Fields parsed out of an existing `STATE.md` for the soft-lock check
/// (spec § 7, decision 3). Only fields that participate in the summarized
/// diff are captured; everything else stays opaque so this parser does not
/// need to track the full schema.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ExistingState {
    pub fork: Option<String>,
    pub confidence: Option<String>,
    pub status: Option<String>,
    pub owner: Option<String>,
    pub quoted_rubric_row: Option<String>,
    pub rubric_version: Option<String>,
}

/// The five files written for a ticket. Returned so the caller can log paths.
#[derive(Debug, Clone)]
pub struct TicketFolderPaths {
    pub folder: PathBuf,
    pub intake: PathBuf,
    pub evidence_preflight: PathBuf,
    pub fork_packet: PathBuf,
    pub drafts: PathBuf,
    pub state: PathBuf,
}

/// Resolve the configured tickets root: `TRIAGE_TICKETS_ROOT` env var,
/// or `./Tickets` if unset.
pub fn tickets_root() -> PathBuf {
    std::env::var(TICKETS_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_TICKETS_ROOT))
}

/// Write the five-markdown ticket folder. `owner` (email/identifier) is
/// recorded in STATE.md. `validator_warnings` come from the LLM call's
/// soft-warn outcome (spec § 6, decision 1).
///
/// Soft-lock semantics (spec § 7, decision 3): if `force` is false and an
/// existing `STATE.md` claims a different owner, returns
/// `TicketFolderError::SoftLockConflict` and writes nothing. Same owner,
/// no existing STATE.md, or `force == true` all proceed.
pub fn write_ticket_folder(
    report: &StructuredTriageReport,
    root: &Path,
    owner: &str,
    validator_warnings: &[String],
    force: bool,
) -> Result<TicketFolderPaths, TicketFolderError> {
    let ticket_id = report.intake.ticket.zendesk_id;
    let folder = root.join(ticket_id.to_string());
    std::fs::create_dir_all(&folder)?;

    let intake = folder.join("INTAKE.md");
    let evidence_preflight = folder.join("EVIDENCE_PREFLIGHT.md");
    let fork_packet = folder.join("FORK_PACKET.md");
    let drafts = folder.join("DRAFTS.md");
    let state = folder.join("STATE.md");

    // Render the new STATE.md upfront so the soft-lock branch can hand the
    // full text to the caller without re-rendering.
    let new_state_content = render_state_md(report, owner, validator_warnings);

    // Soft-lock pre-write check. Order matters: we must not touch any of the
    // five files when a conflict exists.
    if !force {
        if let Some(existing) = read_existing_state(&state) {
            let existing_owner = existing.owner.clone().unwrap_or_default();
            if !existing_owner.is_empty() && existing_owner != owner {
                let summary = compute_state_diff(&existing, report, owner);
                return Err(TicketFolderError::SoftLockConflict {
                    existing_owner,
                    current_owner: owner.to_string(),
                    summary,
                    state_path: state.clone(),
                    new_state_content,
                });
            }
        }
    }

    atomic_write_str(&intake, &render_intake_md(&report.intake))?;
    atomic_write_str(
        &evidence_preflight,
        &render_evidence_preflight_md(&report.evidence_preflight),
    )?;
    atomic_write_str(&fork_packet, &render_fork_packet_md(&report.fork_packet))?;
    atomic_write_str(&drafts, &render_drafts_md(&report.drafts))?;
    atomic_write_str(&state, &new_state_content)?;

    Ok(TicketFolderPaths {
        folder,
        intake,
        evidence_preflight,
        fork_packet,
        drafts,
        state,
    })
}

/// Stash a raw LLM response under `Tickets/<id>/.debug/llm-response-<ts>.json`.
/// Used by the pipeline when `LlmError::StructuredAfterRetry` carries a raw
/// response that should be available for human review (spec § 6, decision 6).
pub fn stash_debug_response(
    root: &Path,
    ticket_id: u64,
    raw_response: &str,
) -> Result<PathBuf, TicketFolderError> {
    let folder = root.join(ticket_id.to_string()).join(".debug");
    std::fs::create_dir_all(&folder)?;
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let path = folder.join(format!("llm-response-{stamp}.json"));
    atomic_write_str(&path, raw_response)?;
    Ok(path)
}

/// Atomically write `contents` to `target` (tempfile + fsync + rename).
/// Reused by `chat.rs` for CONVERSATION.md regeneration and snapshot
/// files; do not extend with ticket-folder-specific logic.
pub(crate) fn atomic_write(target: &Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = target.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("destination has no parent directory: {}", target.display()),
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    use std::io::Write;
    tmp.as_file_mut().write_all(contents)?;
    tmp.as_file_mut().sync_all()?;
    tmp.persist(target).map_err(std::io::Error::other)?;
    Ok(())
}

/// Wrapper around atomic_write for ticket-folder string contents.
fn atomic_write_str(target: &Path, contents: &str) -> Result<(), TicketFolderError> {
    let mut buf = contents.as_bytes().to_vec();
    if !contents.ends_with('\n') {
        buf.push(b'\n');
    }
    atomic_write(target, &buf).map_err(TicketFolderError::Io)
}

//
// ─── INTAKE.md ─────────────────────────────────────────────────────────
//

fn render_intake_md(b: &IntakeBlock) -> String {
    let mut out = String::new();
    out.push_str("# INTAKE\n\n");

    out.push_str("## Housekeeping\n\n");
    let mark = if b.housekeeping_complete { "x" } else { " " };
    out.push_str(&format!("- [{mark}] Ticket directory created.\n"));
    out.push_str(&format!(
        "- [{mark}] Artifacts grouped under the ticket folder.\n"
    ));
    out.push_str(&format!(
        "- [{mark}] No new investigation files left loose.\n\n"
    ));

    let t = &b.ticket;
    out.push_str("## Ticket\n\n");
    out.push_str(&format!("- Zendesk ticket: {}\n", t.zendesk_id));
    out.push_str(&format!("- URL: {}\n", t.url));
    out.push_str(&format!(
        "- Status / priority: {} / {}\n",
        non_empty(&t.status),
        non_empty(&t.priority)
    ));
    out.push_str(&format!("- Tags: {}\n", join_or_dash(&t.tags)));
    out.push_str(&format!("- Requester: {}\n", non_empty(&t.requester)));
    out.push_str(&format!("- Organization: {}\n", non_empty(&t.organization)));
    out.push_str(&format!(
        "- Site / CNC: {} / {}\n",
        opt(&t.site),
        opt(&t.cnc)
    ));
    out.push_str(&format!("- Region: {}\n", opt(&t.region)));
    out.push_str(&format!(
        "- Affected station(s): {}\n",
        join_or_dash(&t.affected_stations)
    ));
    out.push_str(&format!(
        "- Affected agent(s): {}\n",
        join_or_dash(&t.affected_agents)
    ));
    out.push_str(&format!("- Call ID: {}\n", opt(&t.call_id)));
    out.push_str(&format!(
        "- Incident window: {}\n",
        non_empty(&t.incident_window)
    ));
    out.push_str(&format!(
        "- Reported symptom: {}\n\n",
        non_empty(&t.reported_symptom)
    ));

    out.push_str("## One-Line Fingerprint\n\n");
    out.push_str(&format!("`{}`\n\n", non_empty(&b.one_line_fingerprint)));

    out.push_str("## Ticket Summary\n\n");
    if b.ticket_summary.is_empty() {
        out.push_str("_(no summary)_\n\n");
    } else {
        for line in &b.ticket_summary {
            out.push_str(&format!("- {line}\n"));
        }
        out.push('\n');
    }

    out.push_str("## Context Pulls\n\n");
    out.push_str("| Pull | Result | Source |\n|---|---|---|\n");
    if b.context_pulls.is_empty() {
        out.push_str("| _(none)_ | _(none)_ | _(none)_ |\n");
    } else {
        for p in &b.context_pulls {
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                escape_pipe(&p.pull),
                escape_pipe(&p.result),
                escape_pipe(&p.source),
            ));
        }
    }
    out.push('\n');

    out.push_str("## Initial Route\n\n");
    out.push_str(&format!(
        "- Hypothesis: {}\n",
        non_empty(&b.initial_route.hypothesis)
    ));
    out.push_str(&format!(
        "- Justification: {}\n\n",
        non_empty(&b.initial_route.justification)
    ));

    out.push_str("## Intake Decision\n\n");
    out.push_str(&intake_decision_checklist(b.intake_decision));

    out
}

fn intake_decision_checklist(d: IntakeDecision) -> String {
    let mark = |variant: IntakeDecision| if d == variant { "x" } else { " " };
    format!(
        "- [{}] Ready for evidence preflight.\n\
         - [{}] Known issue: add evidence/link to existing owner.\n\
         - [{}] Needs clarification before artifact analysis.\n\
         - [{}] Cannot proceed: missing basic ticket facts.\n",
        mark(IntakeDecision::ReadyForEvidencePreflight),
        mark(IntakeDecision::KnownIssue),
        mark(IntakeDecision::NeedsClarification),
        mark(IntakeDecision::CannotProceed),
    )
}

//
// ─── EVIDENCE_PREFLIGHT.md ─────────────────────────────────────────────
//

fn render_evidence_preflight_md(b: &PreflightBlock) -> String {
    let mut out = String::new();
    out.push_str("# EVIDENCE PREFLIGHT\n\n");

    out.push_str("## Gathered\n\n");
    out.push_str(
        "| ID | Evidence type | Source | Time window | Summary |\n|---|---|---|---|---|\n",
    );
    if b.gathered.is_empty() {
        out.push_str("| _(none)_ | _(none)_ | _(none)_ | _(none)_ | _(none)_ |\n");
    } else {
        for g in &b.gathered {
            let id_cell = if g.id.is_empty() {
                "-".into()
            } else {
                g.id.clone()
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                id_cell,
                escape_pipe(&g.evidence_type),
                escape_pipe(&g.source),
                escape_pipe(&g.time_window),
                escape_pipe(&g.summary),
            ));
        }
    }
    out.push('\n');

    out.push_str("## Decisive evidence\n\n");
    write_bullets_or_none(&mut out, &b.decisive_evidence);

    out.push_str("## Missing / non-decisive evidence\n\n");
    write_bullets_or_none(&mut out, &b.missing_or_non_decisive);

    let _ = &PreflightBlock::default(); // ensure trait import used; cheap
    let _ = (GatheredEvidence::default(), ContextPull::default());

    out
}

//
// ─── FORK_PACKET.md ────────────────────────────────────────────────────
//

fn render_fork_packet_md(p: &ForkPacket) -> String {
    let mut out = String::new();
    out.push_str("# FORK PACKET\n\n");

    let c = &p.commitment;
    out.push_str("## Recommendation\n\n");
    out.push_str(&format!(
        "- Fork: {} — {}\n",
        c.fork_letter.as_str(),
        c.fork_letter.description()
    ));
    out.push_str(&format!("- Confidence: {}\n", c.confidence.as_str()));
    out.push_str(&format!("- Reasoning: {}\n\n", non_empty(&c.reasoning)));

    out.push_str("## Decision Signal\n\n");
    out.push_str(&format!("- Rubric class: {}\n", non_empty(&c.rubric_class)));
    out.push_str(&format!(
        "- Rubric row: \"{}\"\n\n",
        non_empty(&c.quoted_rubric_row)
    ));

    out.push_str("## Evidence Summary\n\n");
    write_bullets_or_none(&mut out, &p.evidence_summary);

    out.push_str("## Missing / Non-Decisive Evidence\n\n");
    write_bullets_or_none(&mut out, &p.missing_evidence);

    out.push_str("## Related Work\n\n");
    let r = &p.related;
    let zd = if r.zendesk.is_empty() {
        "(none)".to_string()
    } else {
        r.zendesk
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let jira = if r.jira.is_empty() {
        "(none)".to_string()
    } else {
        r.jira.join(", ")
    };
    out.push_str(&format!("- Zendesk: {zd}\n"));
    out.push_str(&format!("- Jira: {jira}\n"));
    out.push_str(&format!(
        "- Master ticket: {}\n",
        r.master
            .map(|i| i.to_string())
            .unwrap_or_else(|| "(none)".into())
    ));
    out.push_str(&format!(
        "- Cluster: {}\n\n",
        r.cluster.clone().unwrap_or_else(|| "(none)".into())
    ));

    out.push_str("## Handoff\n\n");
    out.push_str(&handoff_line(
        "Engineering Jira",
        &p.handoff.engineering_jira_needed,
    ));
    out.push_str(&handoff_line(
        "Vendor / Internal IT",
        &p.handoff.vendor_or_it_needed,
    ));
    out.push_str(&handoff_line(
        "Customer note",
        &p.handoff.customer_note_needed,
    ));
    out.push_str(&handoff_line(
        "Internal Zendesk note",
        &p.handoff.internal_note_needed,
    ));

    out
}

fn handoff_line(label: &str, item: &HandoffItem) -> String {
    let yn = if item.needed { "yes" } else { "no" };
    if item.reason.trim().is_empty() {
        format!("- {label}: {yn}.\n")
    } else {
        format!("- {label}: {yn} — {}.\n", item.reason.trim_end_matches('.'))
    }
}

//
// ─── DRAFTS.md ─────────────────────────────────────────────────────────
//

fn render_drafts_md(d: &DraftsBlock) -> String {
    let mut out = String::new();
    out.push_str("# DRAFTS\n\n");
    out.push_str(
        "> **Heads-up.** Every section below is a draft. None of these are \
         posted automatically. Review, edit, then copy-paste into Zendesk / Jira.\n\n",
    );

    out.push_str("## Customer-facing reply\n\n");
    out.push_str("<!-- CONFIRM: edit and paste into a public Zendesk reply -->\n\n");
    out.push_str(&non_empty_block(&d.customer_reply));
    out.push_str("\n\n");

    out.push_str("## Internal Zendesk note\n\n");
    out.push_str("<!-- CONFIRM: edit and paste into an internal Zendesk note -->\n\n");
    out.push_str(&non_empty_block(&d.internal_zendesk_note));
    out.push_str("\n\n");

    out.push_str("## Jira draft (fork A only)\n\n");
    match &d.jira_draft {
        None => {
            out.push_str("_(not applicable — fork is not A, or no Jira required)_\n");
        }
        Some(j) => {
            out.push_str("<!-- CONFIRM: review fields then create the Jira manually -->\n\n");
            out.push_str(&render_jira_draft(j));
        }
    }

    out
}

fn render_jira_draft(j: &JiraDraft) -> String {
    let mut s = String::new();
    s.push_str(&format!("- **Project:** {}\n", non_empty(&j.project)));
    s.push_str(&format!("- **Title:** {}\n", non_empty(&j.title)));
    if let Some(c) = &j.affected_component {
        s.push_str(&format!("- **Affected component:** {}\n", c));
    }
    if let Some(a) = &j.suspected_area {
        s.push_str(&format!("- **Suspected area:** {}\n", a));
    }
    s.push_str("\n**Description:**\n\n");
    s.push_str(&non_empty_block(&j.description));
    s.push('\n');
    if !j.repro_steps.is_empty() {
        s.push_str("\n**Repro steps:**\n\n");
        for (i, step) in j.repro_steps.iter().enumerate() {
            s.push_str(&format!("{}. {step}\n", i + 1));
        }
    }
    s
}

//
// ─── STATE.md ──────────────────────────────────────────────────────────
//

fn render_state_md(
    report: &StructuredTriageReport,
    owner: &str,
    validator_warnings: &[String],
) -> String {
    let c = &report.fork_packet.commitment;
    let r = &report.fork_packet.related;
    let now = fmt_ts(&Utc::now());

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("ticket_id: {}\n", report.intake.ticket.zendesk_id));
    out.push_str(&format!("fork: {}\n", c.fork_letter.as_str()));
    out.push_str(&format!("confidence: {}\n", c.confidence.as_str()));
    out.push_str(&format!(
        "quoted_rubric_row: {}\n",
        yaml_scalar(&c.quoted_rubric_row)
    ));
    out.push_str(&format!(
        "rubric_version: {}\n",
        yaml_scalar(&report.rubric_version)
    ));
    out.push_str(&format!("owner: {}\n", yaml_scalar(owner)));
    out.push_str(&format!("created_at: {now}\n"));
    out.push_str(&format!("updated_at: {now}\n"));
    out.push_str("status: open\n");
    out.push_str("related:\n");
    out.push_str(&format!("  zendesk: {}\n", yaml_int_list(&r.zendesk)));
    out.push_str(&format!("  jira: {}\n", yaml_str_list(&r.jira)));
    out.push_str(&format!(
        "  master: {}\n",
        r.master
            .map(|i| i.to_string())
            .unwrap_or_else(|| "null".into())
    ));
    out.push_str(&format!(
        "cluster: {}\n",
        r.cluster
            .as_deref()
            .map(yaml_scalar)
            .unwrap_or_else(|| "null".into())
    ));
    out.push_str(&format!(
        "validator_warnings: {}\n",
        yaml_str_list(validator_warnings)
    ));
    out.push_str("---\n");
    out
}

//
// ─── helpers ───────────────────────────────────────────────────────────
//

fn write_bullets_or_none(out: &mut String, items: &[String]) {
    if items.is_empty() {
        out.push_str("_(none)_\n\n");
        return;
    }
    for item in items {
        out.push_str(&format!("- {item}\n"));
    }
    out.push('\n');
}

fn non_empty(s: &str) -> String {
    if s.trim().is_empty() {
        "_(unset)_".into()
    } else {
        s.to_string()
    }
}

fn non_empty_block(s: &str) -> String {
    if s.trim().is_empty() {
        "_(empty — fill in before sending)_".into()
    } else {
        s.to_string()
    }
}

fn opt(o: &Option<String>) -> String {
    o.as_deref()
        .map(non_empty)
        .unwrap_or_else(|| "_(unset)_".into())
}

fn join_or_dash(items: &[String]) -> String {
    if items.is_empty() {
        "_(none)_".into()
    } else {
        items.join(", ")
    }
}

fn escape_pipe(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn yaml_scalar(s: &str) -> String {
    // Quote and escape for YAML. Keeps it readable for human edits.
    let escaped: String = s.replace('\\', r"\\").replace('"', r#"\""#);
    format!("\"{escaped}\"")
}

fn yaml_int_list(items: &[u64]) -> String {
    if items.is_empty() {
        return "[]".into();
    }
    let parts: Vec<String> = items.iter().map(|i| i.to_string()).collect();
    format!("[{}]", parts.join(", "))
}

fn yaml_str_list(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".into();
    }
    let parts: Vec<String> = items.iter().map(|s| yaml_scalar(s)).collect();
    format!("[{}]", parts.join(", "))
}

/// Convenience wrapper around `read_existing_state` that returns just the
/// `owner` field. Kept as a test helper for the soft-lock tests.
#[cfg(test)]
pub fn read_state_owner(state_path: &Path) -> Option<String> {
    read_existing_state(state_path)?.owner
}

/// Parse the subset of fields used by the soft-lock from a STATE.md file.
/// Returns `None` if the file cannot be read. Missing fields stay `None`.
///
/// The parser is intentionally narrow: it scans only top-level YAML scalar
/// lines (no nesting) for the keys participating in the summarized diff.
/// Anything nested (`related:` block) is ignored. This is enough for the
/// soft-lock surface; full YAML parsing would pull in another dependency.
pub(crate) fn read_existing_state(state_path: &Path) -> Option<ExistingState> {
    let text = std::fs::read_to_string(state_path).ok()?;
    let mut state = ExistingState::default();
    for line in text.lines() {
        // Top-level keys only: ignore indented (nested) lines like
        // `  zendesk: [...]` under `related:`.
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let (key, value) = match line.split_once(':') {
            Some(pair) => pair,
            None => continue,
        };
        let value = strip_yaml_scalar(value.trim());
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "fork" => state.fork = Some(value),
            "confidence" => state.confidence = Some(value),
            "status" => state.status = Some(value),
            "owner" => state.owner = Some(value),
            "quoted_rubric_row" => state.quoted_rubric_row = Some(value),
            "rubric_version" => state.rubric_version = Some(value),
            _ => {}
        }
    }
    Some(state)
}

/// Strip YAML scalar decoration: surrounding double quotes and a basic
/// `\"` / `\\` unescape. Good enough for values this module writes.
fn strip_yaml_scalar(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        // Reverse the escaping done by `yaml_scalar`.
        return inner.replace(r#"\""#, "\"").replace(r"\\", "\\");
    }
    s.to_string()
}

/// Compute the set of fields that differ between the existing STATE.md and
/// the new structured report. Returned tuples are
/// `(field_name, old_value, new_value)`; fields not present in the existing
/// state are reported with `"(unset)"` as the old value when the new value
/// is non-empty.
pub(crate) fn compute_state_diff(
    existing: &ExistingState,
    new_report: &StructuredTriageReport,
    new_owner: &str,
) -> Vec<(String, String, String)> {
    let c = &new_report.fork_packet.commitment;
    let mut out = Vec::new();
    let pairs: [(&str, Option<&str>, &str); 6] = [
        ("fork", existing.fork.as_deref(), c.fork_letter.as_str()),
        (
            "confidence",
            existing.confidence.as_deref(),
            c.confidence.as_str(),
        ),
        ("status", existing.status.as_deref(), "open"),
        ("owner", existing.owner.as_deref(), new_owner),
        (
            "quoted_rubric_row",
            existing.quoted_rubric_row.as_deref(),
            c.quoted_rubric_row.as_str(),
        ),
        (
            "rubric_version",
            existing.rubric_version.as_deref(),
            new_report.rubric_version.as_str(),
        ),
    ];
    for (name, old, new) in pairs {
        let old_str = old.unwrap_or("");
        if old_str != new {
            out.push((
                name.to_string(),
                if old_str.is_empty() {
                    "(unset)".to_string()
                } else {
                    old_str.to_string()
                },
                new.to_string(),
            ));
        }
    }
    out
}

// Silence unused-import warning during slice 4a; ForkLetter is referenced via
// `c.fork_letter` in render_fork_packet_md and ForkPacket itself in helpers.
const _UNUSED: fn(ForkLetter, &ForkPacket) = |_, _| {};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{
        Confidence, DraftsBlock, ForkCommitment, ForkPacket, GatheredEvidence, HandoffBlock,
        HandoffItem, InitialRoute, IntakeBlock, IntakeDecision, IntakeTicketFacts, JiraDraft,
        PreflightBlock, RelatedWork,
    };
    use tempfile::TempDir;

    fn sample_report() -> StructuredTriageReport {
        StructuredTriageReport {
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
                    reported_symptom: "All consoles flickered black".into(),
                },
                one_line_fingerprint: "JeffCom / us-co-jeffcom-apex / network error / 06:30 UTC"
                    .into(),
                ticket_summary: vec!["Brief multi-console outage".into()],
                context_pulls: vec![ContextPull {
                    pull: "Last related tickets".into(),
                    result: "43874 similar".into(),
                    source: "Zendesk".into(),
                }],
                initial_route: InitialRoute {
                    hypothesis: "Fork B".into(),
                    justification: "Multi-console symptom".into(),
                },
                intake_decision: IntakeDecision::ReadyForEvidencePreflight,
            },
            evidence_preflight: PreflightBlock {
                gathered: vec![GatheredEvidence {
                    id: String::new(),
                    evidence_type: "station log".into(),
                    source: "Jeffcom-74".into(),
                    time_window: "06:30 UTC".into(),
                    summary: "SIP OPTIONS failure".into(),
                }],
                decisive_evidence: vec!["Multi-station flip".into()],
                missing_or_non_decisive: vec!["No AWS Health event".into()],
            },
            fork_packet: ForkPacket {
                commitment: ForkCommitment {
                    fork_letter: ForkLetter::B,
                    confidence: Confidence::Medium,
                    quoted_rubric_row: "customer LAN, switch, or SDWAN. Link to site master ticket"
                        .into(),
                    rubric_class: "Symptom Class 3".into(),
                    reasoning: "Multi-station signal is Class 3 (b)".into(),
                },
                evidence_summary: vec!["20 timeouts in one minute".into()],
                missing_evidence: vec![],
                related: RelatedWork {
                    zendesk: vec![43874, 42708],
                    jira: vec![],
                    master: None,
                    cluster: Some("jeffcom-network-error".into()),
                },
                handoff: HandoffBlock {
                    engineering_jira_needed: HandoffItem {
                        needed: false,
                        reason: "".into(),
                    },
                    vendor_or_it_needed: HandoffItem {
                        needed: true,
                        reason: "Request RCA".into(),
                    },
                    customer_note_needed: HandoffItem {
                        needed: true,
                        reason: "Explain brief interruption".into(),
                    },
                    internal_note_needed: HandoffItem {
                        needed: true,
                        reason: "Document fork B decision".into(),
                    },
                },
            },
            drafts: DraftsBlock {
                customer_reply: "Hi Brandon — we found a brief outage…".into(),
                internal_zendesk_note: "Fork B; rubric row: 'customer LAN…'".into(),
                jira_draft: None,
            },
            rubric_version: "2026-05-13".into(),
        }
    }

    #[test]
    fn writes_five_files() {
        let tmp = TempDir::new().unwrap();
        let r = sample_report();
        let paths = write_ticket_folder(&r, tmp.path(), "test.user@axon.com", &[], false).unwrap();
        for p in [
            &paths.intake,
            &paths.evidence_preflight,
            &paths.fork_packet,
            &paths.drafts,
            &paths.state,
        ] {
            assert!(p.exists(), "missing file: {}", p.display());
        }
        assert_eq!(paths.folder, tmp.path().join("44671"));
    }

    #[test]
    fn intake_md_includes_ticket_facts() {
        let r = sample_report();
        let md = render_intake_md(&r.intake);
        assert!(md.contains("# INTAKE"));
        assert!(md.contains("Zendesk ticket: 44671"));
        assert!(md.contains("Jeffcom-74"));
        assert!(md.contains("us-co-jeffcom-apex"));
        assert!(md.contains("[x] Ready for evidence preflight."));
        assert!(md.contains("[ ] Known issue"));
    }

    #[test]
    fn fork_packet_md_includes_rubric_quote_and_fork_letter() {
        let r = sample_report();
        let md = render_fork_packet_md(&r.fork_packet);
        assert!(md.contains("Fork: B — Vendor or Internal IT"));
        assert!(md.contains("Confidence: medium"));
        assert!(md.contains("customer LAN, switch, or SDWAN"));
        assert!(md.contains("Zendesk: 43874, 42708"));
        assert!(md.contains("Engineering Jira: no"));
        assert!(md.contains("Vendor / Internal IT: yes — Request RCA"));
    }

    #[test]
    fn state_md_emits_yaml_frontmatter_with_warnings_and_owner() {
        let r = sample_report();
        let warnings = vec!["quoted_rubric_row paraphrased".to_string()];
        let md = render_state_md(&r, "owner@example.com", &warnings);
        assert!(md.starts_with("---\n"));
        assert!(md.trim_end().ends_with("---"));
        assert!(md.contains("ticket_id: 44671"));
        assert!(md.contains("fork: B"));
        assert!(md.contains("owner: \"owner@example.com\""));
        assert!(md.contains("status: open"));
        assert!(md.contains("zendesk: [43874, 42708]"));
        assert!(md.contains("validator_warnings: [\"quoted_rubric_row paraphrased\"]"));
        assert!(md.contains("cluster: \"jeffcom-network-error\""));
    }

    #[test]
    fn state_md_handles_no_master_and_no_cluster() {
        let mut r = sample_report();
        r.fork_packet.related.master = None;
        r.fork_packet.related.cluster = None;
        let md = render_state_md(&r, "x@y", &[]);
        assert!(md.contains("master: null"));
        assert!(md.contains("cluster: null"));
        assert!(md.contains("validator_warnings: []"));
    }

    #[test]
    fn drafts_md_marks_no_jira_when_absent() {
        let r = sample_report();
        let md = render_drafts_md(&r.drafts);
        assert!(md.contains("Customer-facing reply"));
        assert!(md.contains("<!-- CONFIRM"));
        assert!(md.contains("not applicable"));
    }

    #[test]
    fn drafts_md_renders_jira_when_present() {
        let mut r = sample_report();
        r.drafts.jira_draft = Some(JiraDraft {
            title: "Fix SBC BYE storm".into(),
            description: "Repeated 500 from SBC at 06:30 UTC".into(),
            affected_component: Some("SBC".into()),
            suspected_area: Some("Kamailio drain".into()),
            repro_steps: vec!["call to 555-...".into()],
            project: "REP".into(),
        });
        let md = render_drafts_md(&r.drafts);
        assert!(md.contains("**Project:** REP"));
        assert!(md.contains("Fix SBC BYE storm"));
        assert!(md.contains("Repro steps"));
    }

    #[test]
    fn evidence_preflight_md_renders_gathered_table() {
        let r = sample_report();
        let md = render_evidence_preflight_md(&r.evidence_preflight);
        assert!(md.contains("| - | station log | Jeffcom-74 |"));
        assert!(md.contains("Multi-station flip"));
        assert!(md.contains("No AWS Health event"));
    }

    #[test]
    fn atomic_write_creates_target() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("out.txt");
        atomic_write_str(&target, "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello\n");
    }

    #[test]
    fn atomic_write_appends_trailing_newline() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("a.txt");
        atomic_write_str(&target, "line\n").unwrap();
        // Should not double up newlines.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "line\n");
    }

    #[test]
    fn stash_debug_response_creates_debug_subfolder() {
        let tmp = TempDir::new().unwrap();
        let p = stash_debug_response(tmp.path(), 44671, "{\"oops\":true}").unwrap();
        assert!(p.exists());
        assert!(p.starts_with(tmp.path().join("44671").join(".debug")));
        assert!(p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("llm-response-"));
    }

    #[test]
    fn read_state_owner_extracts_quoted_value() {
        let tmp = TempDir::new().unwrap();
        let r = sample_report();
        write_ticket_folder(&r, tmp.path(), "alpha@example.com", &[], false).unwrap();
        let owner = read_state_owner(&tmp.path().join("44671").join("STATE.md"));
        assert_eq!(owner.as_deref(), Some("alpha@example.com"));
    }

    #[test]
    fn soft_lock_blocks_overwrite_by_other_owner() {
        let tmp = TempDir::new().unwrap();
        let mut r = sample_report();
        // Alice claims first.
        write_ticket_folder(&r, tmp.path(), "alice@example.com", &[], false).unwrap();
        // Snapshot Alice's INTAKE.md so we can prove no partial write happens.
        let intake_path = tmp.path().join("44671").join("INTAKE.md");
        let alice_intake = std::fs::read_to_string(&intake_path).unwrap();
        // Mutate the report so Bob's would-be INTAKE.md differs byte-for-byte.
        r.intake.one_line_fingerprint = "BOB-OVERWRITE-ATTEMPT".into();
        // Bob attempts to overwrite without --force.
        let err = write_ticket_folder(&r, tmp.path(), "bob@example.com", &[], false).unwrap_err();
        match err {
            TicketFolderError::SoftLockConflict {
                existing_owner,
                current_owner,
                summary,
                state_path,
                new_state_content,
            } => {
                assert_eq!(existing_owner, "alice@example.com");
                assert_eq!(current_owner, "bob@example.com");
                assert!(summary.iter().any(|(k, _, _)| k == "owner"));
                assert!(state_path.ends_with("44671/STATE.md"));
                assert!(new_state_content.contains("owner: \"bob@example.com\""));
            }
            other => panic!("expected SoftLockConflict, got {other:?}"),
        }
        // Atomicity: INTAKE.md must still be Alice's, untouched by Bob's attempt.
        let intake_after = std::fs::read_to_string(&intake_path).unwrap();
        assert_eq!(intake_after, alice_intake);
        assert!(!intake_after.contains("BOB-OVERWRITE-ATTEMPT"));
        // Crucially, STATE.md still names alice.
        let owner = read_state_owner(&tmp.path().join("44671").join("STATE.md")).unwrap();
        assert_eq!(owner, "alice@example.com");
    }

    #[test]
    fn soft_lock_allows_same_owner() {
        let tmp = TempDir::new().unwrap();
        let r = sample_report();
        write_ticket_folder(&r, tmp.path(), "alice@example.com", &[], false).unwrap();
        let paths = write_ticket_folder(&r, tmp.path(), "alice@example.com", &[], false).unwrap();
        assert!(paths.state.exists());
        let owner = read_state_owner(&paths.state).unwrap();
        assert_eq!(owner, "alice@example.com");
    }

    #[test]
    fn soft_lock_allows_force() {
        let tmp = TempDir::new().unwrap();
        let r = sample_report();
        write_ticket_folder(&r, tmp.path(), "alice@example.com", &[], false).unwrap();
        // Bob takes over with force=true.
        let paths = write_ticket_folder(&r, tmp.path(), "bob@example.com", &[], true).unwrap();
        let owner = read_state_owner(&paths.state).unwrap();
        assert_eq!(owner, "bob@example.com");
    }

    #[test]
    fn soft_lock_allows_unclaimed() {
        let tmp = TempDir::new().unwrap();
        let r = sample_report();
        // No prior STATE.md. force=false is fine.
        let paths = write_ticket_folder(&r, tmp.path(), "alice@example.com", &[], false).unwrap();
        assert!(paths.state.exists());
    }

    #[test]
    fn compute_state_diff_shows_only_changed_fields() {
        let mut r = sample_report();
        // Existing state matches everything except fork.
        let existing = ExistingState {
            fork: Some("A".into()),
            confidence: Some(r.fork_packet.commitment.confidence.as_str().into()),
            status: Some("open".into()),
            owner: Some("alice@example.com".into()),
            quoted_rubric_row: Some(r.fork_packet.commitment.quoted_rubric_row.clone()),
            rubric_version: Some(r.rubric_version.clone()),
        };
        // Sanity: report fork is B in the sample.
        assert_eq!(r.fork_packet.commitment.fork_letter.as_str(), "B");
        let diff = compute_state_diff(&existing, &r, "alice@example.com");
        assert_eq!(
            diff.len(),
            1,
            "expected only `fork` to differ, got {diff:?}"
        );
        assert_eq!(diff[0].0, "fork");
        assert_eq!(diff[0].1, "A");
        assert_eq!(diff[0].2, "B");

        // Now change confidence too and verify two diffs.
        r.fork_packet.commitment.confidence = crate::models::Confidence::High;
        let diff2 = compute_state_diff(&existing, &r, "alice@example.com");
        assert_eq!(diff2.len(), 2);
        let names: Vec<&str> = diff2.iter().map(|(n, _, _)| n.as_str()).collect();
        assert!(names.contains(&"fork"));
        assert!(names.contains(&"confidence"));
    }

    #[test]
    fn read_existing_state_parses_all_fields() {
        let tmp = TempDir::new().unwrap();
        let r = sample_report();
        let paths = write_ticket_folder(&r, tmp.path(), "alice@example.com", &[], false).unwrap();
        let parsed = read_existing_state(&paths.state).unwrap();
        assert_eq!(parsed.owner.as_deref(), Some("alice@example.com"));
        assert_eq!(parsed.fork.as_deref(), Some("B"));
        assert_eq!(parsed.status.as_deref(), Some("open"));
        assert_eq!(parsed.rubric_version.as_deref(), Some("2026-05-13"));
        assert!(parsed
            .quoted_rubric_row
            .as_deref()
            .unwrap()
            .contains("customer LAN"));
    }

    #[test]
    fn tickets_root_honors_env_var() {
        // SAFETY: tests run single-threaded in default cargo config, and we
        // restore the env var at the end. If the suite ever moves to parallel
        // env mutation tests, switch to `serial_test`.
        let prev = std::env::var(TICKETS_ROOT_ENV).ok();
        std::env::set_var(TICKETS_ROOT_ENV, "/tmp/custom-tickets");
        assert_eq!(tickets_root(), PathBuf::from("/tmp/custom-tickets"));
        match prev {
            Some(v) => std::env::set_var(TICKETS_ROOT_ENV, v),
            None => std::env::remove_var(TICKETS_ROOT_ENV),
        }
    }
}
