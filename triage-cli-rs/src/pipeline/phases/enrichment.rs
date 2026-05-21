use crate::extract;
use crate::llm;
use crate::models::{LogLine, SiteEntry};

use super::super::ctx::PhaseCtx;
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
                        let extracted_dt = if ctx.opts.anchor_override.is_none() {
                            llm::extract_anchor(ctx.ticket, None).await.unwrap_or(None)
                        } else {
                            None
                        };
                        let (anchor_dt, _src) =
                            extract::resolve_anchor(ctx.ticket, ctx.opts.anchor_override, extracted_dt);
                        let (start, end) = extract::build_window(anchor_dt, ctx.opts.window_minutes)?;
                        let (logs, truncated) =
                            dd.get_logs(&entry.site_name, &ctx.levels, start, end).await?;
                        log_lines = logs;
                        log_truncated = truncated;
                        site_entry = Some(entry);
                    }
                }
                Err(e) => ctx.reporter.phase_failed("enrichment", &e.to_string()),
            }
        }
        ctx.reporter.phase_done(
            "enrichment",
            &format!("{} log line(s)", log_lines.len()),
        );
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
