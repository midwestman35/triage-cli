use crate::datadog::DatadogSource;
use crate::extract;
use crate::llm;
use crate::models::{LogLine, SiteEntry, Ticket};

use super::super::ctx::PhaseCtx;
use super::super::options::InvestigateOptions;
use super::super::site::resolve_site;
use super::super::PipelineError;

pub struct EnrichmentResult {
    pub site_entry: Option<SiteEntry>,
    pub log_lines: Vec<LogLine>,
    pub log_truncated: bool,
}

pub async fn run(ctx: &PhaseCtx<'_>) -> Result<EnrichmentResult, PipelineError> {
    ctx.reporter.phase_started("enrichment", "");
    let mut site_entry: Option<SiteEntry> = None;
    let mut log_lines: Vec<LogLine> = Vec::new();
    let mut log_truncated = false;

    if let Some(dd) = ctx.clients.datadog {
        let sites_path_buf = crate::paths::triage_home().join("data/cnc-map.json");
        let sites_path = sites_path_buf.as_path();
        let mut fetched_logs = false;
        if sites_path.exists() {
            match extract::load_site_map(sites_path) {
                Ok(sites) => {
                    let (resolved, _strategy) = resolve_site(
                        ctx.ticket,
                        &sites,
                        ctx.opts.cnc_override.as_deref(),
                        ctx.opts.site_override.as_deref(),
                        ctx.opts.verbose,
                    )
                    .await?;
                    if let Some(entry) = resolved {
                        let (logs, truncated) = fetch_datadog_logs(
                            dd,
                            ctx.ticket,
                            &ctx.levels,
                            &entry.site_name,
                            ctx.opts,
                        )
                        .await?;
                        log_lines = logs;
                        log_truncated = truncated;
                        site_entry = Some(entry);
                        fetched_logs = true;
                    }
                }
                Err(e) => ctx.reporter.phase_failed("enrichment", &e.to_string()),
            }
        }
        if !fetched_logs && ctx.opts.allow_unscoped_fixture_logs {
            let fallback_site = ctx.opts.site_override.as_deref().unwrap_or("fixture");
            let (logs, truncated) =
                fetch_datadog_logs(dd, ctx.ticket, &ctx.levels, fallback_site, ctx.opts).await?;
            log_lines = logs;
            log_truncated = truncated;
        }
        ctx.reporter
            .phase_done("enrichment", &format!("{} log line(s)", log_lines.len()));
    } else {
        ctx.reporter
            .phase_done("enrichment", "skipped (no Datadog client)");
    }

    Ok(EnrichmentResult {
        site_entry,
        log_lines,
        log_truncated,
    })
}

async fn fetch_datadog_logs(
    dd: &dyn DatadogSource,
    ticket: &Ticket,
    levels: &[String],
    site_name: &str,
    opts: &InvestigateOptions,
) -> Result<(Vec<LogLine>, bool), PipelineError> {
    let extracted_dt = if opts.anchor_override.is_none() {
        llm::extract_anchor(ticket, None).await.unwrap_or(None)
    } else {
        None
    };
    let (anchor_dt, _src) = extract::resolve_anchor(ticket, opts.anchor_override, extracted_dt);
    let (start, end) = extract::build_window(anchor_dt, opts.window_minutes)?;
    dd.get_logs(site_name, levels, start, end)
        .await
        .map_err(PipelineError::from)
}
