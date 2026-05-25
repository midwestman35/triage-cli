use crate::llm::{self, LlmError};
use crate::models::{
    Confidence, DraftsBlock, ForkCommitment, ForkLetter, ForkPacket, HandoffBlock, InitialRoute,
    IntakeBlock, IntakeDecision, IntakeTicketFacts, PreflightBlock, RelatedWork,
    StructuredTriageReport, Ticket, TriageBundle,
};
use crate::ticket_folder;

use super::super::ctx::PhaseCtx;
use super::super::PipelineError;

pub struct LlmOutcome {
    pub report: StructuredTriageReport,
    pub validator_warnings: Vec<String>,
}

fn stub_assess_structured(ticket: &Ticket, rubric_version: &str) -> StructuredTriageReport {
    StructuredTriageReport {
        intake: IntakeBlock {
            housekeeping_complete: true,
            ticket: IntakeTicketFacts {
                zendesk_id: ticket.id,
                url: format!("https://carbyne.zendesk.com/agent/tickets/{}", ticket.id),
                status: "open".into(),
                priority: String::new(),
                tags: ticket.tags.clone(),
                requester: ticket.requester_email.clone().unwrap_or_default(),
                organization: ticket.requester_org.clone().unwrap_or_default(),
                site: None,
                cnc: None,
                region: None,
                affected_stations: Vec::new(),
                affected_agents: Vec::new(),
                call_id: None,
                incident_window: String::new(),
                reported_symptom: ticket.subject.clone(),
            },
            one_line_fingerprint: format!("ZD-{} / stub / --no-llm dry run", ticket.id),
            ticket_summary: vec!["[stub] No LLM call.".into()],
            context_pulls: Vec::new(),
            initial_route: InitialRoute {
                hypothesis: "[stub]".into(),
                justification: "Rerun without --no-llm for a real assessment.".into(),
            },
            intake_decision: IntakeDecision::CannotProceed,
        },
        evidence_preflight: PreflightBlock::default(),
        fork_packet: ForkPacket {
            commitment: ForkCommitment {
                fork_letter: ForkLetter::D,
                confidence: Confidence::Low,
                quoted_rubric_row: "Cannot fork yet".into(),
                rubric_class: "n/a (--no-llm dry run)".into(),
                reasoning: "Stub assessment; LLM call skipped via --no-llm.".into(),
            },
            evidence_summary: Vec::new(),
            missing_evidence: vec![
                "Rerun without --no-llm to obtain an actual fork commitment.".into(),
            ],
            related: RelatedWork::default(),
            handoff: HandoffBlock::default(),
        },
        drafts: DraftsBlock::default(),
        rubric_version: rubric_version.to_string(),
    }
}

pub async fn run(ctx: &PhaseCtx<'_>, bundle: &TriageBundle) -> Result<LlmOutcome, PipelineError> {
    ctx.reporter
        .phase_started("llm_call", "generating structured assessment");
    let (report, validator_warnings, llm_call_metrics) = if ctx.opts.no_llm {
        let r = stub_assess_structured(ctx.ticket, ctx.rubric.version());
        ctx.reporter.phase_done("llm_call", "stub (--no-llm)");
        (r, Vec::new(), None)
    } else {
        let outcome = match llm::triage_structured(
            bundle,
            ctx.rubric,
            None,
            ctx.opts.verbose,
            ctx.opts.redact_enabled,
        )
        .await
        {
            Ok(o) => o,
            Err(e) => {
                // On retry-after failure, stash the raw response under
                // Tickets/<id>/.debug/ before propagating (spec § 6, decision 6).
                if let LlmError::StructuredAfterRetry { raw_response, .. } = &e {
                    let root = ctx
                        .opts
                        .tickets_root
                        .clone()
                        .unwrap_or_else(ticket_folder::tickets_root);
                    match ticket_folder::stash_debug_response(&root, ctx.ticket.id, raw_response) {
                        Ok(p) => ctx.reporter.phase_failed(
                            "llm_call",
                            &format!(
                                "structured validation failed; raw stashed at {}",
                                p.display()
                            ),
                        ),
                        Err(stash_err) => ctx.reporter.phase_failed(
                            "llm_call",
                            &format!(
                                "structured validation failed; stash also failed: {stash_err}"
                            ),
                        ),
                    }
                }
                return Err(PipelineError::Llm(e));
            }
        };
        ctx.reporter.phase_done(
            "llm_call",
            &format!(
                "fork={}, confidence={}",
                outcome.report.fork_packet.commitment.fork_letter.as_str(),
                outcome.report.fork_packet.commitment.confidence.as_str(),
            ),
        );
        let metrics = outcome.llm_metrics.clone();
        (outcome.report, outcome.validator_warnings, Some(metrics))
    };

    // Forward LLM call metrics through the reporter so MetricsReporter can capture them.
    if let Some(ref m) = llm_call_metrics {
        ctx.reporter.record_metric(
            "llm.provider",
            super::super::reporter::MetricValue::Str(m.provider.clone()),
        );
        ctx.reporter.record_metric(
            "llm.model",
            super::super::reporter::MetricValue::Str(m.model.clone()),
        );
        ctx.reporter.record_metric(
            "llm.retried",
            super::super::reporter::MetricValue::Bool(m.retried),
        );
        if let Some(ti) = m.tokens_in {
            ctx.reporter.record_metric(
                "llm.tokens_in",
                super::super::reporter::MetricValue::Int(ti as i64),
            );
        }
        if let Some(to) = m.tokens_out {
            ctx.reporter.record_metric(
                "llm.tokens_out",
                super::super::reporter::MetricValue::Int(to as i64),
            );
        }
    }

    Ok(LlmOutcome {
        report,
        validator_warnings,
    })
}
