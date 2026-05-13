//! End-to-end pipeline that turns a fetched ticket into a five-markdown
//! ticket folder (spec § 4). The `Reporter` trait decouples progress output
//! from orchestration: `StderrReporter` (default), `SilentReporter`
//! (tests/watcher), `ChannelReporter` (TUI).

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use indicatif::{ProgressBar, ProgressStyle};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::datadog::{DatadogClient, DatadogError};
use crate::extract::{self, ExtractError, SiteStrategy};
use crate::llm::{self, LlmError};
use crate::memory;
use crate::models::{
    AnchorSource, Confidence, CustomerHistoryEvidence, DraftsBlock, ForkCommitment, ForkLetter,
    ForkPacket, HandoffBlock, InitialRoute, IntakeBlock, IntakeDecision, IntakeTicketFacts,
    InvestigationSession, LogLine, MemoryContext, PreflightBlock, RelatedWork, SiteEntry,
    StructuredTriageReport, Ticket, TriageBundle,
};
use crate::playbook::Rubric;
use crate::ticket_folder::{self, TicketFolderError, TicketFolderPaths};
use crate::zendesk::{ZendeskClient, ZendeskError};

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error(transparent)]
    Zendesk(#[from] ZendeskError),
    #[error(transparent)]
    Datadog(#[from] DatadogError),
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error(transparent)]
    Extract(#[from] ExtractError),
    #[error(transparent)]
    Memory(#[from] memory::MemoryError),
    #[error(transparent)]
    TicketFolder(#[from] TicketFolderError),
}

/// Progress reporter: decouples display from orchestration. The structured
/// pipeline does not emit a terminal "done" payload through this trait — the
/// caller of `investigate_one_structured` receives the `StructuredInvestigation`
/// return value directly, so a separate done callback is no longer useful.
pub trait Reporter: Send + Sync {
    fn phase_started(&self, phase: &str, detail: &str);
    fn phase_done(&self, phase: &str, detail: &str);
    fn phase_failed(&self, phase: &str, err: &str);
}

#[derive(Default)]
pub struct StderrReporter {
    pub verbose: bool,
}

impl Reporter for StderrReporter {
    fn phase_started(&self, phase: &str, detail: &str) {
        if self.verbose {
            if detail.is_empty() {
                eprintln!("→ {phase}");
            } else {
                eprintln!("→ {phase}: {detail}");
            }
        }
    }
    fn phase_done(&self, phase: &str, detail: &str) {
        if detail.is_empty() {
            eprintln!("✓ {phase}");
        } else {
            eprintln!("✓ {phase}: {detail}");
        }
    }
    fn phase_failed(&self, phase: &str, err: &str) {
        eprintln!("✗ {phase}: {err}");
    }
}

#[derive(Default)]
pub struct SilentReporter;
impl Reporter for SilentReporter {
    fn phase_started(&self, _phase: &str, _detail: &str) {}
    fn phase_done(&self, _phase: &str, _detail: &str) {}
    fn phase_failed(&self, _phase: &str, _err: &str) {}
}

/// Show a braille spinner while `f` runs, when stderr is a TTY.
pub async fn spinner<F, T>(text: &str, show: bool, f: F) -> T
where
    F: std::future::Future<Output = T>,
{
    use std::io::IsTerminal;
    if show && std::io::stderr().is_terminal() {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
        );
        pb.set_message(text.to_string());
        pb.enable_steady_tick(Duration::from_millis(80));
        let result = f.await;
        pb.finish_and_clear();
        result
    } else {
        f.await
    }
}

/// Options bag for `investigate_one_structured`. Avoids long parameter lists at call sites.
#[derive(Debug, Default, Clone)]
pub struct InvestigateOptions {
    pub interactive: bool,
    pub workspace: Option<std::path::PathBuf>,
    pub cnc_override: Option<String>,
    pub site_override: Option<String>,
    pub anchor_override: Option<chrono::DateTime<Utc>>,
    pub window_minutes: i32,
    pub levels: Vec<String>,
    pub verbose: bool,
    pub redact_enabled: bool,
    pub no_llm: bool,
    /// Bypass the `STATE.md` soft-lock when overwriting another analyst's
    /// in-progress investigation. Plumbed from the `--force` CLI flag.
    pub force: bool,
}

impl InvestigateOptions {
    pub fn defaults() -> Self {
        Self {
            interactive: false,
            workspace: None,
            cnc_override: None,
            site_override: None,
            anchor_override: None,
            window_minutes: 30,
            levels: vec!["error".into(), "warn".into(), "info".into()],
            verbose: false,
            redact_enabled: true,
            no_llm: false,
            force: false,
        }
    }
}

/// What `investigate_one_structured` returns when successful.
#[derive(Debug, Clone)]
pub struct StructuredInvestigation {
    pub report: StructuredTriageReport,
    pub paths: TicketFolderPaths,
    pub validator_warnings: Vec<String>,
}

/// Investigation that emits a `StructuredTriageReport` and writes the
/// five-markdown ticket folder. This is the only end-to-end pipeline entry
/// point in v1 — the legacy single-file `triage-notes/` path was removed.
pub async fn investigate_one_structured(
    ticket: Ticket,
    session: &mut InvestigationSession,
    dd_client: Option<&DatadogClient>,
    rubric: &Rubric,
    reporter: &dyn Reporter,
    opts: &InvestigateOptions,
) -> Result<StructuredInvestigation, PipelineError> {
    let levels: Vec<String> = if opts.levels.is_empty() {
        vec!["error".into(), "warn".into(), "info".into()]
    } else {
        opts.levels.clone()
    };

    // Phase: customer_history (same as legacy)
    reporter.phase_started("customer_history", "fetching requester history");
    match ZendeskClient::from_env() {
        Ok(zd) => {
            let email = ticket.requester_email.clone().unwrap_or_default();
            let history = zd.fetch_customer_history(&email, 10).await;
            if !history.is_empty() {
                session.evidence.customer_history = Some(CustomerHistoryEvidence {
                    requester_email: email,
                    tickets: history.clone(),
                    source: "zendesk_customer_history".into(),
                    limit: 10,
                });
            }
            reporter.phase_done(
                "customer_history",
                &format!("{} prior ticket(s) found", history.len()),
            );
        }
        Err(e) => reporter.phase_failed("customer_history", &e.to_string()),
    }

    // Phase: memory_lookup
    reporter.phase_started("memory_lookup", "querying prior investigations");
    let symptom_head: String = ticket.description.chars().take(500).collect();
    let prior =
        memory::retrieve_similar(&ticket.subject, &symptom_head, 3).unwrap_or_default();
    if let Ok(Some(_dup)) = memory::find_duplicate(&ticket.id.to_string()) {
        eprintln!("⚠ ZD-{} was previously investigated", ticket.id);
    }
    let query_tokens = ticket
        .subject
        .to_ascii_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    session.memory_context = Some(MemoryContext {
        entries: prior.clone(),
        query_tokens,
    });
    reporter.phase_done(
        "memory_lookup",
        &format!("{} prior investigation(s) found", prior.len()),
    );

    reporter.phase_started("build_timeline", "");
    reporter.phase_done(
        "build_timeline",
        &format!("{} event(s)", session.timeline.len()),
    );

    // Phase: enrichment (optional Datadog) — same as legacy
    reporter.phase_started("enrichment", "");
    let mut site_entry: Option<SiteEntry> = None;
    let mut log_lines: Vec<LogLine> = Vec::new();
    let mut log_truncated = false;

    if let Some(dd) = dd_client {
        let sites_path = Path::new("data/cnc-map.json");
        if sites_path.exists() {
            match extract::load_site_map(sites_path) {
                Ok(sites) => {
                    let (resolved, _strategy) = resolve_site(
                        &ticket,
                        &sites,
                        opts.cnc_override.as_deref(),
                        opts.site_override.as_deref(),
                        opts.verbose,
                    )
                    .await?;
                    if let Some(entry) = resolved {
                        let extracted_dt = if opts.anchor_override.is_none() {
                            llm::extract_anchor(&ticket, None).await.unwrap_or(None)
                        } else {
                            None
                        };
                        let (anchor_dt, _src) = extract::resolve_anchor(
                            &ticket,
                            opts.anchor_override,
                            extracted_dt,
                        );
                        let (start, end) = extract::build_window(anchor_dt, opts.window_minutes)?;
                        let (logs, truncated) = dd
                            .get_logs(&entry.site_name, &levels, start, end)
                            .await?;
                        log_lines = logs;
                        log_truncated = truncated;
                        site_entry = Some(entry);
                    }
                }
                Err(e) => reporter.phase_failed("enrichment", &e.to_string()),
            }
        }
        reporter.phase_done("enrichment", &format!("{} log line(s)", log_lines.len()));
    } else {
        reporter.phase_done("enrichment", "skipped (no Datadog client)");
    }

    // Phase: llm_call (structured)
    reporter.phase_started("llm_call", "generating structured assessment");
    let (report, validator_warnings) = if opts.no_llm {
        let r = stub_assess_structured(&ticket, rubric.version());
        reporter.phase_done("llm_call", "stub (--no-llm)");
        (r, Vec::new())
    } else {
        let bundle = TriageBundle {
            ticket: ticket.clone(),
            site_entry: site_entry.clone(),
            log_lines: log_lines.clone(),
            log_truncated,
            anchor: Some(opts.anchor_override.unwrap_or(ticket.created_at)),
            anchor_source: Some(if opts.anchor_override.is_some() {
                AnchorSource::Flag
            } else {
                AnchorSource::CreatedAt
            }),
            window_start: None,
            window_end: None,
            downloaded_attachments: session.evidence.attachments.clone(),
            local_files: session.evidence.local_files.clone(),
            pasted_logs: session.evidence.pasted_logs.clone(),
            customer_history: session.evidence.customer_history.clone(),
            memory_context: session.memory_context.clone(),
        };
        let outcome = match llm::triage_structured(
            &bundle,
            rubric,
            None,
            opts.verbose,
            opts.redact_enabled,
        )
        .await
        {
            Ok(o) => o,
            Err(e) => {
                // On retry-after failure, stash the raw response under
                // Tickets/<id>/.debug/ before propagating (spec § 6, decision 6).
                if let LlmError::StructuredAfterRetry { raw_response, .. } = &e {
                    let root = ticket_folder::tickets_root();
                    match ticket_folder::stash_debug_response(&root, ticket.id, raw_response) {
                        Ok(p) => reporter.phase_failed(
                            "llm_call",
                            &format!(
                                "structured validation failed; raw stashed at {}",
                                p.display()
                            ),
                        ),
                        Err(stash_err) => reporter.phase_failed(
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
        reporter.phase_done(
            "llm_call",
            &format!(
                "fork={}, confidence={}",
                outcome.report.fork_packet.commitment.fork_letter.as_str(),
                outcome.report.fork_packet.commitment.confidence.as_str(),
            ),
        );
        (outcome.report, outcome.validator_warnings)
    };

    // Phase: save (five-markdown ticket folder)
    reporter.phase_started("save", "");
    let root = ticket_folder::tickets_root();
    std::fs::create_dir_all(&root).map_err(TicketFolderError::Io)?;
    let owner = current_owner();
    let paths = ticket_folder::write_ticket_folder(
        &report,
        &root,
        &owner,
        &validator_warnings,
        opts.force,
    )?;
    let _ = memory::append_investigation(
        &ticket.id.to_string(),
        ticket.requester_email.as_deref().unwrap_or("unknown"),
        &ticket.subject,
        &ticket.description,
        &report.fork_packet.commitment.reasoning,
        None,
        report.fork_packet.commitment.fork_letter.as_str(),
        &report.fork_packet.commitment.quoted_rubric_row,
        &report.rubric_version,
    );
    reporter.phase_done("save", &format!("→ {}", paths.folder.display()));

    Ok(StructuredInvestigation {
        report,
        paths,
        validator_warnings,
    })
}

/// The current analyst's identifier for `STATE.md`. Falls back through
/// `TRIAGE_OWNER` → `USER` → "unknown" so the soft-lock has a useful value
/// even in headless / CI environments.
fn current_owner() -> String {
    if let Ok(v) = std::env::var("TRIAGE_OWNER") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    if let Ok(v) = std::env::var("USER") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    "unknown".into()
}

/// Stubbed structured report for `--no-llm` dry runs. Produces a `D`
/// (cannot fork yet) outcome with `missing_evidence` populated so a real
/// reviewer can see we short-circuited the LLM call.
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

/// Resolve site with LLM fallback when the deterministic chain returns no_match.
pub async fn resolve_site(
    ticket: &Ticket,
    sites: &[SiteEntry],
    cnc_override: Option<&str>,
    site_override: Option<&str>,
    verbose: bool,
) -> Result<(Option<SiteEntry>, SiteStrategy), PipelineError> {
    let (entry, strategy) =
        extract::lookup_site(ticket, sites, cnc_override, site_override)?;
    if entry.is_some() {
        return Ok((entry, strategy));
    }
    if verbose {
        eprintln!("Site lookup: no_match — asking LLM to identify site");
    }
    let llm_name = match llm::extract_site(ticket, sites, None).await {
        Ok(n) => n,
        Err(e) => {
            if verbose {
                eprintln!("LLM site extraction failed: {e}");
            }
            return Ok((None, SiteStrategy::NoMatch));
        }
    };
    let Some(name) = llm_name else {
        return Ok((None, SiteStrategy::NoMatch));
    };
    let (entry2, _) = extract::lookup_site(ticket, sites, None, Some(&name))?;
    Ok((entry2, SiteStrategy::SiteSubstring))
}

/// Reporter that forwards each phase as an async message to a TUI consumer.
///
/// Note: there is no terminal `Done` event in v1 — the caller of
/// `investigate_one_structured` receives the `StructuredInvestigation` value
/// synchronously. The TUI updates its inbox row by reading `STATE.md` off
/// disk after the call returns.
pub struct ChannelReporter {
    pub tx: mpsc::UnboundedSender<TuiEvent>,
}

#[derive(Debug, Clone)]
pub enum TuiEvent {
    PhaseStarted { phase: String, detail: String },
    PhaseDone { phase: String, detail: String },
    PhaseFailed { phase: String, err: String },
}

impl Reporter for ChannelReporter {
    fn phase_started(&self, phase: &str, detail: &str) {
        let _ = self.tx.send(TuiEvent::PhaseStarted {
            phase: phase.into(),
            detail: detail.into(),
        });
    }
    fn phase_done(&self, phase: &str, detail: &str) {
        let _ = self.tx.send(TuiEvent::PhaseDone {
            phase: phase.into(),
            detail: detail.into(),
        });
    }
    fn phase_failed(&self, phase: &str, err: &str) {
        let _ = self.tx.send(TuiEvent::PhaseFailed {
            phase: phase.into(),
            err: err.into(),
        });
    }
}
