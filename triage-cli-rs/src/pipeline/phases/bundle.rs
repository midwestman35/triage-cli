use crate::models::{AnchorSource, MemoryEntry, TriageBundle};

use super::super::ctx::PhaseCtx;
use super::super::reporter::MetricValue;
use super::enrichment::EnrichmentResult;

pub fn run(
    ctx: &PhaseCtx<'_>,
    prior: &[MemoryEntry],
    enrichment: &EnrichmentResult,
) -> TriageBundle {
    // Record evidence counts before the LLM call (available regardless of --no-llm).
    ctx.reporter.record_metric(
        "evidence.comments",
        MetricValue::Int(ctx.ticket.comments.len() as i64),
    );
    ctx.reporter.record_metric(
        "evidence.attachments",
        MetricValue::Int(
            (ctx.session.evidence.attachments.len()
                + ctx.session.evidence.local_files.len()
                + ctx.session.evidence.pasted_logs.len()) as i64,
        ),
    );
    ctx.reporter.record_metric(
        "evidence.datadog_lines",
        MetricValue::Int(enrichment.log_lines.len() as i64),
    );
    ctx.reporter.record_metric(
        "evidence.memory_hits",
        MetricValue::Int(prior.len() as i64),
    );

    // Build the evidence bundle unconditionally so both the LLM path and the
    // --no-llm path have access to the assigned-ID evidence list. The
    // base-snapshot writes (§ 5.4) consume `bundle.evidence_index`.
    let mut bundle = TriageBundle {
        ticket: ctx.ticket.clone(),
        site_entry: enrichment.site_entry.clone(),
        log_lines: enrichment.log_lines.clone(),
        log_truncated: enrichment.log_truncated,
        anchor: Some(ctx.opts.anchor_override.unwrap_or(ctx.ticket.created_at)),
        anchor_source: Some(if ctx.opts.anchor_override.is_some() {
            AnchorSource::Flag
        } else {
            AnchorSource::CreatedAt
        }),
        window_start: None,
        window_end: None,
        downloaded_attachments: ctx.session.evidence.attachments.clone(),
        local_files: ctx.session.evidence.local_files.clone(),
        pasted_logs: ctx.session.evidence.pasted_logs.clone(),
        customer_history: ctx.session.evidence.customer_history.clone(),
        memory_context: ctx.session.memory_context.clone(),
        evidence_index: Vec::new(),
    };
    bundle.evidence_index = crate::models::assign_evidence_ids(&bundle);
    bundle
}
