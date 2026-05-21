use crate::memory;
use crate::models::{StructuredTriageReport, TriageBundle};
use crate::ticket_folder::{self, TicketFolderError, TicketFolderPaths};

use super::super::base_evidence::collect_base_evidence_entries;
use super::super::ctx::PhaseCtx;
use super::super::owner::current_owner;
use super::super::options::StructuredInvestigation;
use super::super::PipelineError;

pub async fn run(
    ctx: &PhaseCtx<'_>,
    bundle: &TriageBundle,
    report: &StructuredTriageReport,
    validator_warnings: &[String],
) -> Result<(StructuredInvestigation, TicketFolderPaths), PipelineError> {
    ctx.reporter.phase_started("save", "");
    let root = ctx
        .opts
        .tickets_root
        .clone()
        .unwrap_or_else(ticket_folder::tickets_root);
    std::fs::create_dir_all(&root).map_err(TicketFolderError::Io)?;
    let owner = current_owner();
    let paths = ticket_folder::write_ticket_folder(
        report,
        &root,
        &owner,
        validator_warnings,
        ctx.opts.force,
    )?;
    let _ = memory::append_investigation(
        &ctx.ticket.id.to_string(),
        ctx.ticket
            .requester_email
            .as_deref()
            .unwrap_or("unknown"),
        &ctx.ticket.subject,
        &ctx.ticket.description,
        &report.fork_packet.commitment.reasoning,
        None,
        report.fork_packet.commitment.fork_letter.as_str(),
        &report.fork_packet.commitment.quoted_rubric_row,
        &report.rubric_version,
    );
    ctx.reporter
        .phase_done("save", &format!("→ {}", paths.folder.display()));

    // Base snapshots for the interactive investigation feature
    // (spec § 5.4). Skipped when `followup_mode` is true.
    if !ctx.opts.followup_mode {
        let ticket_dir = ctx
            .opts
            .tickets_root
            .clone()
            .unwrap_or_else(ticket_folder::tickets_root)
            .join(ctx.ticket.id.to_string());
        // Best-effort: snapshot write failure is logged but does not
        // fail the investigation. The /revise path treats missing
        // snapshots as a re-fetch trigger.
        if let Err(e) = crate::chat::write_base_ticket(&ticket_dir, ctx.ticket) {
            ctx.reporter.phase_failed("base_ticket_snapshot", &e.to_string());
        }
        let bem = crate::models::BaseEvidenceManifest {
            schema: "triage-cli/base-evidence".into(),
            schema_version: 2,
            ticket_id: ctx.ticket.id.to_string(),
            captured_at: chrono::Utc::now(),
            evidence: collect_base_evidence_entries(bundle),
        };
        if let Err(e) = crate::chat::write_base_evidence_manifest(&ticket_dir, &bem) {
            ctx.reporter.phase_failed("base_evidence_snapshot", &e.to_string());
        }
    }

    Ok((
        StructuredInvestigation {
            report: report.clone(),
            paths: paths.clone(),
            validator_warnings: validator_warnings.to_vec(),
        },
        paths,
    ))
}
