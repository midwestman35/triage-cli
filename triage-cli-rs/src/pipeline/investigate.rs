use crate::datadog::DatadogSource;
use crate::models::{InvestigationSession, Ticket};
use crate::playbook::Rubric;
use crate::zendesk::ZendeskSource;

use super::ctx::{Clients, PhaseCtx};
use super::options::{InvestigateOptions, StructuredInvestigation};
use super::phases;
use super::reporter::Reporter;
use super::PipelineError;

/// Investigation that emits a `StructuredTriageReport` and writes the
/// five-markdown ticket folder. This is the primary structured pipeline
/// entry point (the legacy single-file `triage-notes/` path was removed),
/// but it is no longer the *only* public pipeline entry point: [`revise`]
/// re-runs this pipeline (with `followup_mode=true`) to rewrite the
/// five-markdown folder from base snapshots, and [`followup_turn`] drives
/// the conversational chat turns. (Doc-comment portion of #26; the
/// CLAUDE.md / AGENTS.md prose is corrected in a separate PR.)
pub async fn investigate_one_structured(
    ticket: Ticket,
    session: &mut InvestigationSession,
    zd_client: Option<&dyn ZendeskSource>,
    dd_client: Option<&dyn DatadogSource>,
    rubric: &Rubric,
    reporter: &dyn Reporter,
    opts: &InvestigateOptions,
) -> Result<StructuredInvestigation, PipelineError> {
    let levels: Vec<String> = if opts.levels.is_empty() {
        vec!["error".into(), "warn".into(), "info".into()]
    } else {
        opts.levels.clone()
    };

    let mut ctx = PhaseCtx {
        ticket: &ticket,
        session,
        opts,
        rubric,
        reporter,
        clients: Clients {
            zendesk: zd_client,
            datadog: dd_client,
        },
        levels,
    };

    phases::history::run(&mut ctx).await;
    let prior = phases::memory::run(&mut ctx);
    phases::timeline::run(&ctx);
    let enrichment = phases::enrichment::run(&ctx).await?;
    let bundle = phases::bundle::run(&ctx, &prior, &enrichment);
    let llm_outcome = phases::llm::run(&ctx, &bundle).await?;
    let (investigation, _paths) = phases::save::run(
        &ctx,
        &bundle,
        &llm_outcome.report,
        &llm_outcome.validator_warnings,
    )
    .await?;

    Ok(investigation)
}
