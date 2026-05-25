use crate::models::{
    fmt_ts, indent_continuations, truncate_head_tail, AttachmentEvidence, LocalFileEvidence,
    PastedEvidence, TriageBundle,
};

const EVIDENCE_HEAD_BYTES: usize = 32_000;
const EVIDENCE_TAIL_BYTES: usize = 8_000;

/// Render the bundle into the user-message string sent to the LLM.
///
/// This is serialization for the LLM boundary, not a domain-model concern. The
/// shape and section ordering must match the original Python `as_user_message`
/// implementation byte-for-byte so existing system prompts behave the same.
pub(crate) fn render_user_message(bundle: &TriageBundle) -> String {
    let mut lines: Vec<String> = Vec::new();
    let t = &bundle.ticket;

    let tags_str = if t.tags.is_empty() {
        "(none)".to_string()
    } else {
        t.tags.join(", ")
    };
    let org_str = t
        .requester_org
        .clone()
        .unwrap_or_else(|| "(unset)".to_string());

    // Memory context — inject prior investigations before ticket body.
    if let Some(ctx) = &bundle.memory_context {
        if !ctx.entries.is_empty() {
            lines.push("## Prior investigations (top similar)".into());
            for e in &ctx.entries {
                lines.push(format!("{} | {} | {}", e.ticket_id, e.customer, e.subject));
                lines.push(format!("  Assessment: {}", e.assessment));
                if !e.resolution.is_empty() && e.resolution != "[unknown]" {
                    lines.push(format!("  Resolution: {}", e.resolution));
                }
            }
            lines.push(String::new());
        }
    }

    // Customer ticket history.
    if let Some(history) = &bundle.customer_history {
        if !history.tickets.is_empty() {
            lines.push("## Customer ticket history (recent)".into());
            for tk in &history.tickets {
                lines.push(format!(
                    "{} | {:<8} | {} | {}",
                    tk.id,
                    tk.status,
                    tk.updated_at.format("%Y-%m-%d"),
                    tk.subject
                ));
            }
            lines.push(String::new());
        }
    }

    if let Some(s) = &bundle.site_entry {
        lines.push("# Customer".into());
        lines.push(format!("- Friendly name: {}", s.friendly_name));
        lines.push(format!("- Site: {}", s.site_name));
        lines.push(format!("- CNC: {}", s.cnc));
        lines.push(String::new());
    }

    lines.push(format!("# Ticket #{}", t.id));
    lines.push(format!("Subject: {}", t.subject));
    lines.push(format!("Created: {}", fmt_ts(&t.created_at)));
    lines.push(format!("Requester org: {org_str}"));
    lines.push(format!("Tags: {tags_str}"));
    lines.push(String::new());
    lines.push("## Description".into());
    lines.push(indent_continuations(&t.description));
    lines.push(String::new());
    lines.push("## Comments (chronological; \"[internal]\" prefix for non-public)".into());
    if t.comments.is_empty() {
        lines.push("(no comments)".into());
    } else {
        for c in &t.comments {
            let prefix = if c.is_public { "" } else { "[internal] " };
            let body = indent_continuations(&c.body);
            lines.push(format!(
                "- {prefix}{} — {}: {body}",
                fmt_ts(&c.created_at),
                c.author
            ));
        }
    }
    lines.push(String::new());

    let n = bundle.log_lines.len();
    match (
        bundle.anchor.as_ref(),
        bundle.anchor_source.as_ref(),
        bundle.window_start.as_ref(),
        bundle.window_end.as_ref(),
    ) {
        (Some(anchor), Some(src), Some(start), Some(end)) => {
            let truncated_str = if bundle.log_truncated {
                ", truncated"
            } else {
                ""
            };
            lines.push(format!(
                "# Logs (anchor: {} from {}; window: {} to {}; {} lines{})",
                fmt_ts(anchor),
                src.as_str(),
                fmt_ts(start),
                fmt_ts(end),
                n,
                truncated_str
            ));
        }
        _ => {
            lines.push(format!("# Logs ({n} lines; no Datadog window)"));
        }
    }

    if bundle.log_lines.is_empty() {
        lines.push("(no logs in window)".into());
    } else {
        for log in &bundle.log_lines {
            let msg = indent_continuations(&log.message);
            lines.push(format!(
                "- {} [{}] {msg}",
                fmt_ts(&log.timestamp),
                log.level
            ));
        }
    }

    // Supplemental evidence section (only when populated).
    let has_evidence = !bundle.downloaded_attachments.is_empty()
        || !bundle.local_files.is_empty()
        || !bundle.pasted_logs.is_empty();
    if has_evidence {
        lines.push(String::new());
        lines.push("# Supplemental Evidence".into());
        if !bundle.downloaded_attachments.is_empty() {
            lines.push(String::new());
            lines.push("## Downloaded attachments".into());
            for a in &bundle.downloaded_attachments {
                extend_with_attachment(&mut lines, a);
            }
        }
        if !bundle.local_files.is_empty() {
            lines.push(String::new());
            lines.push("## Local files (analyst-supplied)".into());
            for lf in &bundle.local_files {
                extend_with_local_file(&mut lines, lf);
            }
        }
        if !bundle.pasted_logs.is_empty() {
            lines.push(String::new());
            lines.push("## Pasted evidence".into());
            for p in &bundle.pasted_logs {
                extend_with_pasted(&mut lines, p);
            }
        }
    }

    // Evidence index — appended last so the LLM can reference IDs while writing the report.
    if !bundle.evidence_index.is_empty() {
        lines.push(String::new());
        lines.push("## Evidence Index".into());
        lines.push(
            "(Cite `E-NNN` IDs in `gathered[*].id`, `evidence_summary`, \
             and `decisive_evidence` bullets)"
                .into(),
        );
        lines.push(String::new());
        lines.push("| ID | Type | Label | Time |".into());
        lines.push("|---|---|---|---|".into());
        for item in &bundle.evidence_index {
            let ts = item
                .source_time
                .as_ref()
                .map(fmt_ts)
                .unwrap_or_else(|| "-".into());
            let label = item.label.replace('|', "\\|").replace('\n', " ");
            lines.push(format!(
                "| {} | {} | {} | {} |",
                item.id, item.kind, label, ts
            ));
        }
    }

    lines.join("\n")
}

fn fmt_size(size_bytes: Option<u64>) -> String {
    match size_bytes {
        Some(b) => format!("{b} bytes"),
        None => "unknown size".into(),
    }
}

fn extend_with_attachment(out: &mut Vec<String>, a: &AttachmentEvidence) {
    let size = fmt_size(a.size_bytes);
    let ctype = a
        .content_type
        .clone()
        .unwrap_or_else(|| "unknown type".into());
    out.push(format!("- {} ({ctype}, {size})", a.filename));
    match &a.extracted_text {
        Some(text) => {
            let truncated = truncate_head_tail(text, EVIDENCE_HEAD_BYTES, EVIDENCE_TAIL_BYTES);
            let trimmed = truncated.trim_end_matches('\n').to_string();
            out.push(indent_continuations(&format!("  {trimmed}")));
        }
        None => out.push("  (binary, not extracted)".into()),
    }
}

fn extend_with_local_file(out: &mut Vec<String>, lf: &LocalFileEvidence) {
    let size = fmt_size(lf.size_bytes);
    let dtype = lf
        .detected_type
        .map(|t| t.as_str().to_string())
        .unwrap_or_else(|| "unknown".into());
    let name = lf
        .path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| lf.path.display().to_string());
    out.push(format!("- {name} ({dtype}, {size})"));
    match &lf.extracted_text {
        Some(text) => {
            let truncated = truncate_head_tail(text, EVIDENCE_HEAD_BYTES, EVIDENCE_TAIL_BYTES);
            let trimmed = truncated.trim_end_matches('\n').to_string();
            out.push(indent_continuations(&format!("  {trimmed}")));
        }
        None => out.push("  (binary, not extracted)".into()),
    }
}

fn extend_with_pasted(out: &mut Vec<String>, p: &PastedEvidence) {
    let truncated = truncate_head_tail(&p.text, EVIDENCE_HEAD_BYTES, EVIDENCE_TAIL_BYTES);
    let trimmed = truncated.trim_end_matches('\n').to_string();
    out.push(format!("- {}", p.label));
    out.push(indent_continuations(&format!("  {trimmed}")));
}
