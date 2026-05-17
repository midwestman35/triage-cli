//! End-to-end pipeline that turns a fetched ticket into a five-markdown
//! ticket folder (spec § 4). The `Reporter` trait decouples progress output
//! from orchestration: `StderrReporter` (default), `SilentReporter`
//! (tests/watcher), `ChannelReporter` (TUI).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::Utc;
use indicatif::{ProgressBar, ProgressStyle};
use thiserror::Error;
use tokio::sync::mpsc;

use crate::datadog::{DatadogError, DatadogSource};
use crate::extract::{self, ExtractError, SiteStrategy};
use crate::llm::{self, LlmError};
use crate::memory;
use crate::models::{
    AnchorSource, Confidence, CustomerHistoryEvidence, DraftsBlock, ForkCommitment, ForkLetter,
    ForkPacket, HandoffBlock, InitialRoute, IntakeBlock, IntakeDecision, IntakeTicketFacts,
    InvestigationSession, LogLine, MemoryContext, MemoryEntry, PreflightBlock, RelatedWork,
    SiteEntry, StructuredTriageReport, Ticket, TriageBundle,
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
    #[error("followup: {0}")]
    Followup(#[from] FollowupError),
}

#[derive(Debug, Error)]
pub enum FollowupError {
    #[error("session lost and replay also failed: {0}")]
    SessionLostNoReplay(String),
    #[error("could not capture codex session id from output")]
    CodexSessionCaptureFailed,
    #[error("lock contention at {0}")]
    LockContention(PathBuf),
    #[error("base snapshot missing or unreadable: {0}")]
    BaseSnapshotMissing(String),
    #[error(transparent)]
    Chat(#[from] crate::chat::ChatError),
    #[error(transparent)]
    Provider(#[from] crate::providers::ProviderError),
}

/// Typed value emitted via `Reporter::record_metric`. Kept simple — the only
/// consumers today are `MetricsReporter` (captures for JSON) and the default
/// no-op on all other implementations.
#[derive(Debug, Clone)]
pub enum MetricValue {
    Float(f64),
    Int(i64),
    Bool(bool),
    Str(String),
}

/// Progress reporter: decouples display from orchestration. The structured
/// pipeline does not emit a terminal "done" payload through this trait — the
/// caller of `investigate_one_structured` receives the `StructuredInvestigation`
/// return value directly, so a separate done callback is no longer useful.
pub trait Reporter: Send + Sync {
    fn phase_started(&self, phase: &str, detail: &str);
    fn phase_done(&self, phase: &str, detail: &str);
    fn phase_failed(&self, phase: &str, err: &str);
    /// Record a named metric for observability. Default is a no-op so existing
    /// reporters (`StderrReporter`, `SilentReporter`, `ChannelReporter`) need
    /// no changes. Only `MetricsReporter` captures these.
    fn record_metric(&self, _key: &str, _value: MetricValue) {}
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
    /// Pre-loaded customer history (fixture/demo mode). When set, the pipeline
    /// skips the live Zendesk customer-history fetch.
    pub customer_history_override: Option<CustomerHistoryEvidence>,
    /// Pre-loaded memory hits (fixture/demo mode). When set, the pipeline
    /// skips the live SQLite BM25 lookup.
    pub memory_hits_override: Option<Vec<MemoryEntry>>,
    /// Set to `true` when this run is a `/revise` followup rather than the
    /// original investigation. Suppresses the base-snapshot writes (spec § 5.4)
    /// so the original snapshots are never overwritten by a revision run.
    /// Task 12 sets this flag; all existing call sites leave it at the
    /// `Default::default()` value of `false`.
    pub followup_mode: bool,
    /// Override for the ticket-folder write destination. When `None`, the
    /// pipeline falls back to `ticket_folder::tickets_root()` (which reads
    /// `TRIAGE_TICKETS_ROOT`). When `Some(p)`, the ticket folder is written
    /// under `p` regardless of the env var. /revise uses this to write under
    /// the existing ticket_dir's parent without mutating process-global env.
    pub tickets_root: Option<std::path::PathBuf>,
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
            customer_history_override: None,
            memory_hits_override: None,
            followup_mode: false,
            tickets_root: None,
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

    // Phase: customer_history
    reporter.phase_started("customer_history", "fetching requester history");
    if let Some(history_override) = opts.customer_history_override.clone() {
        let count = history_override.tickets.len();
        session.evidence.customer_history = Some(history_override);
        reporter.phase_done(
            "customer_history",
            &format!("{count} prior ticket(s) (fixture)"),
        );
    } else {
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
    }

    // Phase: memory_lookup
    reporter.phase_started("memory_lookup", "querying prior investigations");
    let symptom_head: String = ticket.description.chars().take(500).collect();
    let prior = if let Some(hits) = opts.memory_hits_override.clone() {
        hits
    } else {
        memory::retrieve_similar(&ticket.subject, &symptom_head, 3).unwrap_or_default()
    };
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
                        let (anchor_dt, _src) =
                            extract::resolve_anchor(&ticket, opts.anchor_override, extracted_dt);
                        let (start, end) = extract::build_window(anchor_dt, opts.window_minutes)?;
                        let (logs, truncated) =
                            dd.get_logs(&entry.site_name, &levels, start, end).await?;
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

    // Record evidence counts before the LLM call (available regardless of --no-llm).
    reporter.record_metric(
        "evidence.comments",
        MetricValue::Int(ticket.comments.len() as i64),
    );
    reporter.record_metric(
        "evidence.attachments",
        MetricValue::Int(
            (session.evidence.attachments.len()
                + session.evidence.local_files.len()
                + session.evidence.pasted_logs.len()) as i64,
        ),
    );
    reporter.record_metric(
        "evidence.datadog_lines",
        MetricValue::Int(log_lines.len() as i64),
    );
    reporter.record_metric("evidence.memory_hits", MetricValue::Int(prior.len() as i64));

    // Build the evidence bundle unconditionally so both the LLM path and the
    // --no-llm path have access to the assigned-ID evidence list. The
    // base-snapshot writes (§ 5.4) consume `bundle.evidence_index`.
    let mut bundle = TriageBundle {
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
        evidence_index: Vec::new(),
    };
    bundle.evidence_index = crate::models::assign_evidence_ids(&bundle);

    // Phase: llm_call (structured)
    reporter.phase_started("llm_call", "generating structured assessment");
    let (report, validator_warnings, llm_call_metrics) = if opts.no_llm {
        let r = stub_assess_structured(&ticket, rubric.version());
        reporter.phase_done("llm_call", "stub (--no-llm)");
        (r, Vec::new(), None)
    } else {
        let outcome =
            match llm::triage_structured(&bundle, rubric, None, opts.verbose, opts.redact_enabled)
                .await
            {
                Ok(o) => o,
                Err(e) => {
                    // On retry-after failure, stash the raw response under
                    // Tickets/<id>/.debug/ before propagating (spec § 6, decision 6).
                    if let LlmError::StructuredAfterRetry { raw_response, .. } = &e {
                        let root = opts
                            .tickets_root
                            .clone()
                            .unwrap_or_else(ticket_folder::tickets_root);
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
        let metrics = outcome.llm_metrics.clone();
        (outcome.report, outcome.validator_warnings, Some(metrics))
    };

    // Forward LLM call metrics through the reporter so MetricsReporter can capture them.
    if let Some(ref m) = llm_call_metrics {
        reporter.record_metric("llm.provider", MetricValue::Str(m.provider.clone()));
        reporter.record_metric("llm.model", MetricValue::Str(m.model.clone()));
        reporter.record_metric("llm.retried", MetricValue::Bool(m.retried));
        if let Some(ti) = m.tokens_in {
            reporter.record_metric("llm.tokens_in", MetricValue::Int(ti as i64));
        }
        if let Some(to) = m.tokens_out {
            reporter.record_metric("llm.tokens_out", MetricValue::Int(to as i64));
        }
    }

    // Phase: save (five-markdown ticket folder)
    reporter.phase_started("save", "");
    let root = opts
        .tickets_root
        .clone()
        .unwrap_or_else(ticket_folder::tickets_root);
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

    // Base snapshots for the interactive investigation feature
    // (spec § 5.4). Skipped when `followup_mode` is true.
    if !opts.followup_mode {
        let ticket_dir = opts
            .tickets_root
            .clone()
            .unwrap_or_else(ticket_folder::tickets_root)
            .join(ticket.id.to_string());
        // Best-effort: snapshot write failure is logged but does not
        // fail the investigation. The /revise path treats missing
        // snapshots as a re-fetch trigger.
        if let Err(e) = crate::chat::write_base_ticket(&ticket_dir, &ticket) {
            reporter.phase_failed("base_ticket_snapshot", &e.to_string());
        }
        let bem = crate::models::BaseEvidenceManifest {
            schema: "triage-cli/base-evidence".into(),
            schema_version: 1,
            ticket_id: ticket.id.to_string(),
            captured_at: chrono::Utc::now(),
            evidence: bundle.evidence_index.clone(),
        };
        if let Err(e) = crate::chat::write_base_evidence_manifest(&ticket_dir, &bem) {
            reporter.phase_failed("base_evidence_snapshot", &e.to_string());
        }
    }

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
    let (entry, strategy) = extract::lookup_site(ticket, sites, cnc_override, site_override)?;
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

/// Shows a braille spinner on stderr while each pipeline phase is running.
/// TTY-gated: falls back to inner `phase_started` when stderr is not a terminal
/// so tests, the watcher, and piped runs are unaffected.
pub struct SpinnerReporter {
    inner: Box<dyn Reporter>,
    current: Mutex<Option<ProgressBar>>,
}

impl SpinnerReporter {
    pub fn new(inner: Box<dyn Reporter>) -> Self {
        Self {
            inner,
            current: Mutex::new(None),
        }
    }

    fn clear_current(&self) {
        if let Some(pb) = self.current.lock().unwrap().take() {
            pb.finish_and_clear();
        }
    }
}

impl Reporter for SpinnerReporter {
    fn phase_started(&self, phase: &str, detail: &str) {
        use std::io::IsTerminal;
        self.clear_current();
        if !std::io::stderr().is_terminal() {
            self.inner.phase_started(phase, detail);
            return;
        }
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
        );
        let msg = if detail.is_empty() {
            phase.to_string()
        } else {
            format!("{phase}: {detail}")
        };
        pb.set_message(msg);
        pb.enable_steady_tick(Duration::from_millis(80));
        *self.current.lock().unwrap() = Some(pb);
    }

    fn phase_done(&self, phase: &str, detail: &str) {
        self.clear_current();
        self.inner.phase_done(phase, detail);
    }

    fn phase_failed(&self, phase: &str, err: &str) {
        self.clear_current();
        self.inner.phase_failed(phase, err);
    }
}

/// Wraps another reporter and captures phase timings plus named metrics.
/// Pass `&MetricsReporter` as the `&dyn Reporter` to the pipeline; after the
/// call returns, read collected data via `phase_timings()` and `named_metrics()`.
pub struct MetricsReporter {
    inner: Box<dyn Reporter>,
    phase_starts: Mutex<HashMap<String, Instant>>,
    phase_timings: Mutex<HashMap<String, f64>>,
    named: Mutex<Vec<(String, MetricValue)>>,
}

impl MetricsReporter {
    pub fn new(inner: Box<dyn Reporter>) -> Self {
        Self {
            inner,
            phase_starts: Mutex::new(HashMap::new()),
            phase_timings: Mutex::new(HashMap::new()),
            named: Mutex::new(Vec::new()),
        }
    }

    /// Wall-clock seconds per phase (keyed by phase name).
    pub fn phase_timings(&self) -> HashMap<String, f64> {
        self.phase_timings.lock().unwrap().clone()
    }

    /// Ordered list of (key, value) pairs recorded via `record_metric`.
    pub fn named_metrics(&self) -> Vec<(String, MetricValue)> {
        self.named.lock().unwrap().clone()
    }
}

impl Reporter for MetricsReporter {
    fn phase_started(&self, phase: &str, detail: &str) {
        self.phase_starts
            .lock()
            .unwrap()
            .insert(phase.to_string(), Instant::now());
        self.inner.phase_started(phase, detail);
    }

    fn phase_done(&self, phase: &str, detail: &str) {
        let elapsed = self
            .phase_starts
            .lock()
            .unwrap()
            .remove(phase)
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        self.phase_timings
            .lock()
            .unwrap()
            .insert(phase.to_string(), elapsed);
        self.inner.phase_done(phase, detail);
    }

    fn phase_failed(&self, phase: &str, err: &str) {
        let elapsed = self
            .phase_starts
            .lock()
            .unwrap()
            .remove(phase)
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        self.phase_timings
            .lock()
            .unwrap()
            .insert(phase.to_string(), elapsed);
        self.inner.phase_failed(phase, err);
    }

    fn record_metric(&self, key: &str, value: MetricValue) {
        self.named
            .lock()
            .unwrap()
            .push((key.to_string(), value.clone()));
        self.inner.record_metric(key, value);
    }
}

/// Append a follow-up turn pair (analyst question + provider response)
/// to the conversation log under `ticket_dir`. Does NOT mutate the
/// five-markdown folder — that only happens on /revise (see
/// `investigate_one_structured` with `followup_mode=true`, Task 12).
///
/// Acquires the per-ticket lock for both writes (analyst turn + provider
/// turn). The caller is expected to have already validated `prompt` (e.g.
/// rendered it from analyst input + attached evidence bodies).
pub async fn followup_turn(
    ticket_dir: &std::path::Path,
    ticket_id: &str,
    prompt: &str,
    system_prompt: &str,
    model: &str,
    attachments: &[crate::models::Attachment],
    provider: &dyn crate::providers::LlmProvider,
) -> Result<crate::providers::FollowupResult, PipelineError> {
    use crate::chat;
    use std::time::Duration;

    // Acquire the per-ticket lock BEFORE reading conversation state. Reading
    // outside the lock allows a concurrent writer to append between our read
    // and our lock-acquire, yielding a stale `next_turn` and a duplicate turn
    // number on append.
    let conv = chat::conversation_jsonl_path(ticket_dir);
    let session_dir = chat::session_dir(ticket_dir);
    let _guard =
        chat::acquire_session_lock(&session_dir, Duration::from_secs(5)).map_err(|e| match e {
            crate::chat::ChatError::LockContention { lock_path } => {
                FollowupError::LockContention(lock_path)
            }
            other => FollowupError::Chat(other),
        })?;

    // Read existing turns under the lock to determine next turn number + session id.
    let outcome = chat::parse_conversation_jsonl(&conv).map_err(FollowupError::from)?;
    let last_codex_session = outcome
        .turns
        .iter()
        .rev()
        .find_map(|t| t.session_id.clone());
    let next_turn = outcome.turns.iter().map(|t| t.turn).max().unwrap_or(0) + 1;

    // Apply PII redaction at the LLM boundary (spec § 7.1g, § 9.3).
    // The redactor scrubs caller PII (phones, addresses, GPS coords);
    // operational identifiers (Call-IDs, station codes, CNC UUIDs,
    // site names) are preserved.
    let (redacted_prompt, _redaction_counts) = crate::redact::redact(prompt);

    // Call provider
    let started = std::time::Instant::now();
    let result = provider
        .followup(
            last_codex_session.as_deref(),
            &redacted_prompt,
            system_prompt,
            model,
            attachments,
        )
        .await
        .map_err(FollowupError::Provider)?;
    let elapsed_s = started.elapsed().as_secs_f64();

    // Append the provider turn
    let provider_turn = crate::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: ticket_id.to_string(),
        turn: next_turn,
        turn_kind: crate::models::TurnKind::Codex,
        ts: chrono::Utc::now(),
        author: None,
        body: result.text.clone(),
        evidence: vec![],
        provider: Some(provider.name().to_string()),
        model: Some(model.to_string()),
        tokens_in: result.tokens_in,
        tokens_out: result.tokens_out,
        elapsed_s: Some(elapsed_s),
        session_id: result.session_id.clone(),
        resumed: Some(result.resumed),
        action: None,
        outcome: None,
        drove_revision_from_turns: None,
        diff: None,
    };
    chat::append_turn(&conv, &provider_turn).map_err(FollowupError::Chat)?;

    // Re-render markdown
    let parsed = chat::parse_conversation_jsonl(&conv).map_err(FollowupError::from)?;
    chat::write_conversation_md(
        &chat::conversation_md_path(ticket_dir),
        &parsed.turns,
        ticket_id,
    )
    .map_err(FollowupError::from)?;

    // Update manifest (best-effort — failure here is logged but not fatal)
    if let Ok(Some(mut m)) = chat::read_session_manifest_opt(ticket_dir) {
        if result.session_id.is_some() {
            m.resume_count = m.resume_count.saturating_add(1);
            m.last_resumed_at = Some(chrono::Utc::now());
            let _ = chat::write_session_manifest(ticket_dir, &m);
        }
    } else {
        // First follow-up: create the manifest
        let m = crate::models::SessionManifest {
            version: 1,
            provider: provider.name().to_string(),
            model: model.to_string(),
            created_at: chrono::Utc::now(),
            last_resumed_at: None,
            resume_count: 0,
            codex_capture_method: None,
        };
        let _ = chat::write_session_manifest(ticket_dir, &m);
    }

    Ok(result)
}

/// Build the synthetic `InvestigationSession` that `/revise` feeds into
/// `investigate_one_structured`. Seeds the session with a catalog summary
/// of the base evidence (so the LLM is aware of the original signal),
/// then layers post-base evidence from analyst/automated turns recorded
/// since `last_revise_turn`.
///
/// **Why a catalog summary, not full content:** `BaseEvidenceManifest`
/// stores `EvidenceItem`s — IDs + kind + label + source pointer — not the
/// underlying bodies. Datadog log lines, file content, and paste bodies
/// live in their original sources. We surface the catalog so the
/// re-emission preserves the original `E-NNN` identifiers and labels;
/// fully restoring content would require extending the manifest schema to
/// store body snapshots.
///
/// Extracted into a free function so it can be unit-tested directly (the
/// no-llm pipeline path stubs the output and doesn't reveal what the
/// session actually contains).
fn build_revise_session(
    base_ticket: &crate::models::Ticket,
    base_evidence: &crate::models::BaseEvidenceManifest,
    turns: &[crate::models::Turn],
    last_revise_turn: u32,
) -> crate::models::InvestigationSession {
    let mut session = crate::investigation::create_session(base_ticket.clone());

    // 1. Surface base-evidence catalog as a labeled paste so the LLM
    //    re-emission knows what evidence existed originally.
    if !base_evidence.evidence.is_empty() {
        let mut catalog = String::from(
            "Base-evidence catalog from the original investigation \
             (bodies live in their original sources — not re-fetched here):\n",
        );
        for item in &base_evidence.evidence {
            catalog.push_str(&format!("- {} [{}] {}\n", item.id, item.kind, item.label));
        }
        crate::investigation::add_pasted_evidence(&mut session, "base-evidence-catalog", &catalog);
    }

    // 2. Layer post-base evidence from turns since the last revise.
    for t in turns.iter().filter(|t| t.turn > last_revise_turn) {
        for ev in &t.evidence {
            match ev {
                crate::models::EvidenceProvenance::File { copied_path, .. } => {
                    // Best-effort: if the copied file has been removed we skip it.
                    let _ = crate::investigation::add_local_file(&mut session, copied_path);
                }
                crate::models::EvidenceProvenance::Paste { label, body, .. } => {
                    crate::investigation::add_pasted_evidence(&mut session, label, body);
                }
            }
        }
        // Also feed analyst-turn body text as a labeled paste so the
        // structured pipeline sees what the analyst told us.
        if matches!(t.turn_kind, crate::models::TurnKind::Analyst) && !t.body.is_empty() {
            crate::investigation::add_pasted_evidence(
                &mut session,
                &format!("turn-{:03}-body", t.turn),
                &t.body,
            );
        }
    }

    session
}

/// `/revise` re-entry. Validates that there is new evidence since the
/// last revise, loads base snapshots, builds a synthetic
/// `InvestigationSession`, and calls `investigate_one_structured` with
/// `followup_mode=true` to re-emit the five-markdown folder.  Then
/// appends a system revise turn to CONVERSATION.jsonl so the chat pane
/// shows the revision event.
///
/// `opts` is forwarded to `investigate_one_structured`; the caller should
/// set `no_llm: true` in tests / dry-run mode.  `followup_mode` is
/// always forced to `true` regardless of what `opts` contains.
pub async fn revise(
    ticket_dir: &std::path::Path,
    ticket_id: &str,
    dd_client: Option<&dyn DatadogSource>,
    opts: &InvestigateOptions,
) -> Result<(), PipelineError> {
    use crate::chat;
    use std::time::Duration;

    // Acquire the per-ticket lock for the duration of the revise.
    let session_dir = chat::session_dir(ticket_dir);
    let _guard =
        chat::acquire_session_lock(&session_dir, Duration::from_secs(5)).map_err(|e| match e {
            chat::ChatError::LockContention { lock_path } => {
                FollowupError::LockContention(lock_path)
            }
            other => FollowupError::Chat(other),
        })?;

    // Load the conversation and find the last system/revise turn number.
    let conv = chat::conversation_jsonl_path(ticket_dir);
    let outcome = chat::parse_conversation_jsonl(&conv).map_err(FollowupError::Chat)?;
    let last_revise_turn = outcome
        .turns
        .iter()
        .rev()
        .find(|t| {
            matches!(t.turn_kind, crate::models::TurnKind::System)
                && t.action.as_deref() == Some("revise")
        })
        .map(|t| t.turn)
        .unwrap_or(0);

    // Validate: at least one new analyst-or-automated turn since the last
    // revise must carry new evidence (file or labeled paste). A
    // question-only turn does NOT qualify.
    let new_evidence_present = outcome.turns.iter().any(|t| {
        t.turn > last_revise_turn
            && matches!(
                t.turn_kind,
                crate::models::TurnKind::Analyst | crate::models::TurnKind::Automated
            )
            && !t.evidence.is_empty()
    });
    if !new_evidence_present {
        return Err(PipelineError::Followup(FollowupError::BaseSnapshotMissing(
            "no new evidence since last /revise; attach a file or labeled paste before revising"
                .to_string(),
        )));
    }

    // Load base snapshots.
    let base_ticket = chat::read_base_ticket(ticket_dir)
        .map_err(|e| FollowupError::BaseSnapshotMissing(e.to_string()))?;
    let base_evidence = chat::read_base_evidence_manifest(ticket_dir)
        .map_err(|e| FollowupError::BaseSnapshotMissing(e.to_string()))?;

    // --- Structured re-emission (spec § 2.5 V1 ship list) ---
    // Build a synthetic InvestigationSession from the base ticket, the base
    // evidence catalog, and any new evidence (file attachments and labeled
    // pastes) extracted from CONVERSATION.jsonl turns since the last revise.
    // Then run the structured pipeline with followup_mode=true so the
    // five-markdown folder is rewritten while the base snapshots are preserved.
    let mut session = build_revise_session(
        &base_ticket,
        &base_evidence,
        &outcome.turns,
        last_revise_turn,
    );

    let rubric = crate::playbook::Rubric::load()
        .map_err(|e| FollowupError::BaseSnapshotMissing(format!("rubric load failed: {e}")))?;

    // Build the options for the structured re-emission. followup_mode is always
    // true on /revise. `force` is propagated from the caller so /revise honors
    // the STATE.md owner soft-lock (the per-ticket session lock acquired above
    // is orthogonal — it serializes concurrent writers on this ticket, but
    // does NOT authorize one analyst to overwrite another's STATE.md).
    //
    // `tickets_root` is set to the existing ticket_dir's parent so that the
    // structured pipeline writes back to the same folder we're revising,
    // without mutating process-global TRIAGE_TICKETS_ROOT.
    let tickets_root_parent = ticket_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(ticket_folder::tickets_root);
    let revise_opts = InvestigateOptions {
        followup_mode: true,
        force: opts.force,
        tickets_root: Some(tickets_root_parent),
        no_llm: opts.no_llm,
        redact_enabled: opts.redact_enabled,
        verbose: opts.verbose,
        memory_hits_override: opts.memory_hits_override.clone(),
        customer_history_override: opts.customer_history_override.clone(),
        ..InvestigateOptions::defaults()
    };

    let reporter = SilentReporter;
    // `dd_client` is forwarded so callers that have a live DatadogSource
    // (e.g. the inbox /revise handler) can re-fetch logs around the original
    // anchor. When `None`, the pipeline skips DD entirely and relies on the
    // base-evidence catalog plus whatever the analyst attached this turn.
    let _structured = investigate_one_structured(
        base_ticket.clone(),
        &mut session,
        dd_client,
        &rubric,
        &reporter,
        &revise_opts,
    )
    .await?;

    let next_turn = outcome.turns.iter().map(|t| t.turn).max().unwrap_or(0) + 1;
    let driving_turns: Vec<u32> = outcome
        .turns
        .iter()
        .filter(|t| t.turn > last_revise_turn)
        .map(|t| t.turn)
        .collect();

    let system_turn = crate::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: ticket_id.to_string(),
        turn: next_turn,
        turn_kind: crate::models::TurnKind::System,
        ts: chrono::Utc::now(),
        author: None,
        body: format!(
            "Revise validated using base ticket id {}. {} turn(s) since last revise carry new evidence.",
            base_ticket.id,
            driving_turns.len(),
        ),
        evidence: vec![],
        provider: None,
        model: None,
        tokens_in: None,
        tokens_out: None,
        elapsed_s: None,
        session_id: None,
        resumed: None,
        action: Some("revise".to_string()),
        outcome: Some("validated".to_string()),
        drove_revision_from_turns: Some(driving_turns),
        diff: None,
    };
    chat::append_turn(&conv, &system_turn).map_err(FollowupError::Chat)?;

    let parsed = chat::parse_conversation_jsonl(&conv).map_err(FollowupError::Chat)?;
    chat::write_conversation_md(
        &chat::conversation_md_path(ticket_dir),
        &parsed.turns,
        ticket_id,
    )
    .map_err(FollowupError::Chat)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::{FixtureDatadogClient, FixtureLoader};
    use crate::playbook::Rubric;

    /// Run the pipeline against the audio-drop fixture (ticket #55001) using
    /// `no_llm: true` so no network calls are made. Returns the pipeline result.
    ///
    /// Acquires the shared memory env-guard so that this test is serialised
    /// with every `memory::tests::*` test that also touches
    /// `TRIAGE_MEMORY_MD` / `TRIAGE_MEMORY_DB`.  All three process-global
    /// env vars are overridden to paths inside `tickets_root` and restored
    /// on return.
    async fn run_fixture_pipeline(
        tickets_root: &std::path::Path,
    ) -> Result<StructuredInvestigation, PipelineError> {
        // Point the fixture loader at the crate's bundled fixtures directory.
        let fixtures_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let loader = FixtureLoader::new(fixtures_dir.join("audio-drop"))
            .expect("audio-drop fixture must exist");
        let ticket = loader.load_ticket().expect("fixture ticket.json");
        let logs = loader
            .load_datadog_logs()
            .expect("fixture datadog-logs.json");
        let memory_hits = loader.load_memory_hits().expect("fixture memory-hits.json");

        let mut session = crate::investigation::create_session(ticket.clone());
        let fixture_dd = FixtureDatadogClient::new(logs);
        let opts = InvestigateOptions {
            no_llm: true,
            memory_hits_override: Some(memory_hits),
            force: true, // avoid soft-lock conflicts between parallel test runs
            followup_mode: false,
            ..InvestigateOptions::defaults()
        };
        let rubric = Rubric::load().expect("embedded rubric must parse");
        let reporter = SilentReporter;

        // Override TRIAGE_TICKETS_ROOT, TRIAGE_MEMORY_MD, and TRIAGE_MEMORY_DB
        // to paths inside this test's tempdir, and hold the process-wide env
        // mutex for the duration so we don't race with memory::tests::*.
        let memory_md = tickets_root.join("MEMORY.md");
        let memory_db = tickets_root.join("data/memory.db");
        let _env = crate::memory::MemoryEnvScope::new_with_tickets_root(
            &memory_md,
            &memory_db,
            Some(tickets_root),
        );

        investigate_one_structured(
            ticket,
            &mut session,
            Some(&fixture_dd as &dyn crate::datadog::DatadogSource),
            &rubric,
            &reporter,
            &opts,
        )
        .await
    }

    #[tokio::test]
    async fn investigate_writes_base_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_fixture_pipeline(dir.path()).await;
        assert!(
            outcome.is_ok(),
            "fixture pipeline failed: {:?}",
            outcome.err()
        );

        // Ticket #55001 is the audio-drop fixture id
        let ticket_dir = dir.path().join("55001");

        // Both snapshots must exist after a successful non-followup run
        assert!(
            ticket_dir.join(".session/base-ticket.json").exists(),
            "base-ticket.json not written"
        );
        assert!(
            ticket_dir
                .join(".session/base-evidence-manifest.json")
                .exists(),
            "base-evidence-manifest.json not written"
        );

        // Round-trip the snapshots to confirm they are valid JSON
        let bt =
            crate::chat::read_base_ticket(&ticket_dir).expect("base-ticket.json must round-trip");
        let _ = bt.id; // round-trip asserted by successful read

        let bem = crate::chat::read_base_evidence_manifest(&ticket_dir)
            .expect("base-evidence-manifest.json must round-trip");
        let _ = bem.ticket_id; // round-trip asserted by successful read
    }

    #[tokio::test]
    async fn followup_turn_appends_to_conversation_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        std::fs::create_dir_all(&ticket_dir).unwrap();

        // Seed an analyst turn-001
        let analyst = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44776".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: chrono::Utc::now(),
            author: Some("enrique".into()),
            body: "what's up?".into(),
            evidence: vec![],
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
        };
        let conv = crate::chat::conversation_jsonl_path(&ticket_dir);
        {
            let _guard = crate::chat::acquire_session_lock(
                &crate::chat::session_dir(&ticket_dir),
                std::time::Duration::from_secs(1),
            )
            .unwrap();
            crate::chat::append_turn(&conv, &analyst).unwrap();
        }

        // Fake provider that returns canned text
        struct FakeProvider;
        impl crate::providers::LlmProvider for FakeProvider {
            fn name(&self) -> &'static str {
                "fake"
            }
            fn complete<'a>(
                &'a self,
                _prompt: &'a str,
                _sys: &'a str,
                _model: &'a str,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                crate::providers::CompletionResult,
                                crate::providers::ProviderError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async move {
                    Ok(crate::providers::CompletionResult {
                        text: "fake codex reply".into(),
                        tokens_in: Some(100),
                        tokens_out: Some(50),
                    })
                })
            }
        }

        let provider: Box<dyn crate::providers::LlmProvider> = Box::new(FakeProvider);
        let result = followup_turn(
            &ticket_dir,
            "44776",
            "follow-up question",
            "system",
            "fake-model",
            &[],
            provider.as_ref(),
        )
        .await
        .unwrap();

        assert!(result.text.contains("fake codex reply"));

        // Conversation now has turn-001 analyst + turn-002 codex
        let parsed = crate::chat::parse_conversation_jsonl(&conv).unwrap();
        assert_eq!(parsed.turns.len(), 2);
        assert!(matches!(
            parsed.turns[1].turn_kind,
            crate::models::TurnKind::Codex
        ));
    }

    #[tokio::test]
    async fn followup_turn_applies_pii_redaction() {
        // Verifies that phone numbers in the analyst prompt are scrubbed before
        // reaching the provider (spec § 7.1g, § 9.3).
        use std::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44777");
        std::fs::create_dir_all(&ticket_dir).unwrap();

        // Seed an analyst turn-001 so the conversation file exists
        let analyst = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44777".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: chrono::Utc::now(),
            author: Some("enrique".into()),
            body: "initial question".into(),
            evidence: vec![],
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
        };
        let conv = crate::chat::conversation_jsonl_path(&ticket_dir);
        {
            let _guard = crate::chat::acquire_session_lock(
                &crate::chat::session_dir(&ticket_dir),
                std::time::Duration::from_secs(1),
            )
            .unwrap();
            crate::chat::append_turn(&conv, &analyst).unwrap();
        }

        // FakeProvider that records the exact prompt it received
        struct CapturingProvider {
            captured_prompt: Mutex<Option<String>>,
        }
        impl crate::providers::LlmProvider for CapturingProvider {
            fn name(&self) -> &'static str {
                "fake-capturing"
            }
            fn complete<'a>(
                &'a self,
                prompt: &'a str,
                _sys: &'a str,
                _model: &'a str,
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                crate::providers::CompletionResult,
                                crate::providers::ProviderError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async move {
                    *self.captured_prompt.lock().unwrap() = Some(prompt.to_string());
                    Ok(crate::providers::CompletionResult {
                        text: "redaction-test reply".into(),
                        tokens_in: Some(5),
                        tokens_out: Some(5),
                    })
                })
            }
        }

        // Prompt contains a phone number that should be scrubbed.
        // Uses the same pattern as redact::tests::redacts_phone.
        let prompt_with_pii = "call (555) 123-4567 for the incident report";

        let provider = CapturingProvider {
            captured_prompt: Mutex::new(None),
        };
        followup_turn(
            &ticket_dir,
            "44777",
            prompt_with_pii,
            "system",
            "fake-model",
            &[],
            &provider,
        )
        .await
        .unwrap();

        let captured = provider
            .captured_prompt
            .lock()
            .unwrap()
            .clone()
            .expect("provider must have been called");

        // The raw phone number must NOT appear in what the provider received.
        assert!(
            !captured.contains("555") || !captured.contains("123-4567"),
            "PII phone number leaked to provider; captured prompt: {captured:?}"
        );
        // The redaction sentinel MUST appear instead.
        assert!(
            captured.contains("<PHONE>"),
            "expected <PHONE> sentinel in redacted prompt; got: {captured:?}"
        );
    }

    #[tokio::test]
    async fn revise_uses_base_ticket_snapshot_and_preserves_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        std::fs::create_dir_all(ticket_dir.join(".session")).unwrap();

        // Seed a base-ticket and base-evidence snapshot
        let ticket = crate::models::Ticket {
            id: 44776,
            subject: "audio dropped".into(),
            description: "".into(),
            requester_org: None,
            requester_email: None,
            tags: vec![],
            created_at: chrono::Utc::now(),
            updated_at: None,
            comments: vec![],
        };
        crate::chat::write_base_ticket(&ticket_dir, &ticket).unwrap();
        crate::chat::write_base_evidence_manifest(
            &ticket_dir,
            &crate::models::BaseEvidenceManifest {
                schema: "triage-cli/base-evidence".into(),
                schema_version: 1,
                ticket_id: "44776".into(),
                captured_at: chrono::Utc::now(),
                evidence: vec![],
            },
        )
        .unwrap();

        // Seed an analyst follow-up turn WITH evidence (a paste)
        let analyst = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44776".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: chrono::Utc::now(),
            author: Some("enrique".into()),
            body: "new evidence: reboot at 14:32".into(),
            evidence: vec![crate::models::EvidenceProvenance::Paste {
                label: "note".into(),
                body: "reboot evidence".into(),
                bytes: 16,
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
        };
        let conv_path = crate::chat::conversation_jsonl_path(&ticket_dir);
        crate::chat::append_turn(&conv_path, &analyst).unwrap();
        let analyst_pre = crate::chat::parse_conversation_jsonl(&conv_path).unwrap();
        assert_eq!(analyst_pre.turns.len(), 1);

        // Hold the memory env scope so investigate_one_structured doesn't
        // try to open the real SQLite DB.
        let memory_md = dir.path().join("MEMORY.md");
        let memory_db = dir.path().join("data/memory.db");
        let _env = crate::memory::MemoryEnvScope::new_with_tickets_root(
            &memory_md,
            &memory_db,
            Some(dir.path()),
        );

        // Call revise with no_llm=true (stub pipeline; no LLM API needed)
        let no_llm_opts = InvestigateOptions {
            no_llm: true,
            memory_hits_override: Some(vec![]),
            ..InvestigateOptions::defaults()
        };
        let outcome = revise(&ticket_dir, "44776", None, &no_llm_opts).await;
        assert!(outcome.is_ok(), "revise failed: {:?}", outcome.err());

        // Conversation must be preserved + extended with a system revise turn
        let after = crate::chat::parse_conversation_jsonl(&conv_path).unwrap();
        assert!(after.turns.len() >= 2);
        let last = after.turns.last().unwrap();
        assert!(matches!(last.turn_kind, crate::models::TurnKind::System));
        assert_eq!(last.action.as_deref(), Some("revise"));
    }

    #[tokio::test]
    async fn end_to_end_revise_against_fixture() {
        // Set up a self-contained end-to-end state in a tempdir mirroring
        // what the spec § 7.2 example would produce. This is a unit-test
        // version of the fixture (the actual fixture is deferred to v2).

        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        std::fs::create_dir_all(ticket_dir.join(".session")).unwrap();

        // Seed base-ticket.json and base-evidence-manifest.json
        let ticket = crate::models::Ticket {
            id: 44776,
            subject: "audio dropped".into(),
            description: "initial".into(),
            requester_org: None,
            requester_email: None,
            tags: vec![],
            created_at: chrono::Utc::now(),
            updated_at: None,
            comments: vec![],
        };
        crate::chat::write_base_ticket(&ticket_dir, &ticket).unwrap();
        crate::chat::write_base_evidence_manifest(
            &ticket_dir,
            &crate::models::BaseEvidenceManifest {
                schema: "triage-cli/base-evidence".into(),
                schema_version: 1,
                ticket_id: "44776".into(),
                captured_at: chrono::Utc::now(),
                evidence: vec![],
            },
        )
        .unwrap();

        // Seed an analyst follow-up turn WITH evidence
        let analyst = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44776".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: chrono::Utc::now(),
            author: Some("enrique".into()),
            body: "new log shows reboot at 14:32".into(),
            evidence: vec![crate::models::EvidenceProvenance::Paste {
                label: "customer-note".into(),
                body: "reboot at 14:32 PT".into(),
                bytes: 18,
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
        };
        let conv_path = crate::chat::conversation_jsonl_path(&ticket_dir);
        crate::chat::append_turn(&conv_path, &analyst).unwrap();

        // Hold the memory env scope so investigate_one_structured doesn't
        // try to open the real SQLite DB.
        let memory_md = dir.path().join("MEMORY.md");
        let memory_db = dir.path().join("data/memory.db");
        let _env = crate::memory::MemoryEnvScope::new_with_tickets_root(
            &memory_md,
            &memory_db,
            Some(dir.path()),
        );

        // Call /revise with no_llm=true (stub pipeline; no LLM API needed)
        let no_llm_opts = InvestigateOptions {
            no_llm: true,
            memory_hits_override: Some(vec![]),
            ..InvestigateOptions::defaults()
        };
        revise(&ticket_dir, "44776", None, &no_llm_opts)
            .await
            .expect("revise must succeed");

        // Conversation now has: analyst turn-001 + system revise turn-002
        let parsed = crate::chat::parse_conversation_jsonl(&conv_path).unwrap();
        assert_eq!(parsed.turns.len(), 2);
        let last = parsed.turns.last().unwrap();
        assert!(matches!(last.turn_kind, crate::models::TurnKind::System));
        assert_eq!(last.action.as_deref(), Some("revise"));
        assert!(
            last.drove_revision_from_turns
                .as_ref()
                .map(|v| v.contains(&1))
                .unwrap_or(false),
            "drove_revision_from_turns must include turn 1"
        );

        // CONVERSATION.md should also exist and contain both turns
        let md_path = crate::chat::conversation_md_path(&ticket_dir);
        assert!(md_path.exists(), "CONVERSATION.md was not written");
        let md = std::fs::read_to_string(&md_path).unwrap();
        assert!(
            md.contains("turn-001 analyst"),
            "CONVERSATION.md missing turn-001 analyst header"
        );
        assert!(
            md.contains("turn-002 system"),
            "CONVERSATION.md missing turn-002 system header"
        );
    }

    #[tokio::test]
    async fn revise_respects_soft_lock_when_force_unset() {
        // /revise must NOT silently overwrite another analyst's STATE.md.
        // Pre-seed a STATE.md whose owner differs from the current process
        // owner, then call revise() with opts.force = false. The pipeline
        // must surface a SoftLockConflict error and leave the existing
        // STATE.md intact.
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44889");
        std::fs::create_dir_all(ticket_dir.join(".session")).unwrap();

        let ticket = crate::models::Ticket {
            id: 44889,
            subject: "soft-lock guard".into(),
            description: "".into(),
            requester_org: None,
            requester_email: None,
            tags: vec![],
            created_at: chrono::Utc::now(),
            updated_at: None,
            comments: vec![],
        };
        crate::chat::write_base_ticket(&ticket_dir, &ticket).unwrap();
        crate::chat::write_base_evidence_manifest(
            &ticket_dir,
            &crate::models::BaseEvidenceManifest {
                schema: "triage-cli/base-evidence".into(),
                schema_version: 1,
                ticket_id: "44889".into(),
                captured_at: chrono::Utc::now(),
                evidence: vec![],
            },
        )
        .unwrap();

        let analyst = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44889".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: chrono::Utc::now(),
            author: Some("enrique".into()),
            body: "new evidence: log dump".into(),
            evidence: vec![crate::models::EvidenceProvenance::Paste {
                label: "note".into(),
                body: "fresh evidence".into(),
                bytes: 14,
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
        };
        let conv_path = crate::chat::conversation_jsonl_path(&ticket_dir);
        crate::chat::append_turn(&conv_path, &analyst).unwrap();

        // Pre-seed STATE.md with a foreign owner. Use a sentinel value that
        // will never collide with the test machine's $USER.
        let foreign_state = "---\n\
ticket_id: 44889\n\
fork: B\n\
confidence: low\n\
quoted_rubric_row: \"\"\n\
rubric_version: \"2026-04-30\"\n\
owner: \"foreign-owner-not-this-test@triage.test\"\n\
created_at: 2026-05-13T07:32:11Z\n\
updated_at: 2026-05-13T07:32:11Z\n\
status: open\n\
related:\n  zendesk: []\n  jira: []\n  master: null\n\
cluster: null\n\
validator_warnings: []\n---\n";
        std::fs::write(ticket_dir.join("STATE.md"), foreign_state).unwrap();

        let memory_md = dir.path().join("MEMORY.md");
        let memory_db = dir.path().join("data/memory.db");
        let _env = crate::memory::MemoryEnvScope::new_with_tickets_root(
            &memory_md,
            &memory_db,
            Some(dir.path()),
        );

        let opts = InvestigateOptions {
            no_llm: true,
            force: false,
            memory_hits_override: Some(vec![]),
            ..InvestigateOptions::defaults()
        };
        let outcome = revise(&ticket_dir, "44889", None, &opts).await;

        assert!(
            matches!(
                outcome,
                Err(PipelineError::TicketFolder(
                    TicketFolderError::SoftLockConflict { .. }
                ))
            ),
            "expected SoftLockConflict, got {outcome:?}"
        );

        // The pre-seeded STATE.md must remain untouched on conflict.
        let post = std::fs::read_to_string(ticket_dir.join("STATE.md")).unwrap();
        assert!(
            post.contains("foreign-owner-not-this-test@triage.test"),
            "STATE.md was overwritten on soft-lock conflict"
        );
    }

    #[tokio::test]
    async fn revise_does_not_mutate_tickets_root_env() {
        // /revise must not leave TRIAGE_TICKETS_ROOT in a different state than
        // it found it — that mutation leaks into concurrent inbox/watch work
        // running in the same process. The fix routes the destination through
        // `InvestigateOptions::tickets_root` instead of process-global env.
        let dir = tempfile::tempdir().unwrap();
        let other_root = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44890");
        std::fs::create_dir_all(ticket_dir.join(".session")).unwrap();

        let ticket = crate::models::Ticket {
            id: 44890,
            subject: "tickets-root guard".into(),
            description: "".into(),
            requester_org: None,
            requester_email: None,
            tags: vec![],
            created_at: chrono::Utc::now(),
            updated_at: None,
            comments: vec![],
        };
        crate::chat::write_base_ticket(&ticket_dir, &ticket).unwrap();
        crate::chat::write_base_evidence_manifest(
            &ticket_dir,
            &crate::models::BaseEvidenceManifest {
                schema: "triage-cli/base-evidence".into(),
                schema_version: 1,
                ticket_id: "44890".into(),
                captured_at: chrono::Utc::now(),
                evidence: vec![],
            },
        )
        .unwrap();

        let analyst = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44890".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: chrono::Utc::now(),
            author: Some("enrique".into()),
            body: "fresh paste".into(),
            evidence: vec![crate::models::EvidenceProvenance::Paste {
                label: "note".into(),
                body: "evidence".into(),
                bytes: 8,
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
        };
        let conv_path = crate::chat::conversation_jsonl_path(&ticket_dir);
        crate::chat::append_turn(&conv_path, &analyst).unwrap();

        // Set TRIAGE_TICKETS_ROOT to a path that is NOT the ticket's parent.
        // The mutation bug overwrites this to ticket_dir.parent(); a working
        // refactor leaves the env untouched.
        let memory_md = dir.path().join("MEMORY.md");
        let memory_db = dir.path().join("data/memory.db");
        let _env = crate::memory::MemoryEnvScope::new_with_tickets_root(
            &memory_md,
            &memory_db,
            Some(other_root.path()),
        );

        let env_before = std::env::var("TRIAGE_TICKETS_ROOT").unwrap();
        assert_eq!(
            env_before,
            other_root.path().to_string_lossy(),
            "test setup did not seed the env correctly"
        );

        let opts = InvestigateOptions {
            no_llm: true,
            force: true, // bypass any soft-lock from a prior test
            tickets_root: Some(ticket_dir.parent().unwrap().to_path_buf()),
            memory_hits_override: Some(vec![]),
            ..InvestigateOptions::defaults()
        };
        revise(&ticket_dir, "44890", None, &opts)
            .await
            .expect("revise must succeed");

        let env_after = std::env::var("TRIAGE_TICKETS_ROOT").unwrap();
        assert_eq!(
            env_after, env_before,
            "revise mutated TRIAGE_TICKETS_ROOT from {env_before:?} to {env_after:?}"
        );

        // Sanity: writes still landed where requested (ticket_dir.parent()), not
        // under the env-set other_root.
        assert!(
            ticket_dir.join("STATE.md").exists(),
            "STATE.md was not written at ticket_dir; opts.tickets_root not honored"
        );
    }

    #[test]
    fn build_revise_session_surfaces_base_evidence_catalog() {
        // /revise must surface the base-evidence catalog (E-NNN ids + labels
        // recorded during the original investigation) so the LLM knows what
        // signal originally drove the fork. The manifest stores only a
        // catalog — bodies live in source files / external systems — so we
        // can't fully restore content, but we can pass the labeled list
        // forward as a synthetic context paste.
        //
        // Post-base evidence (file/paste added in follow-up turns) must also
        // make it into the session.
        let base_ticket = crate::models::Ticket {
            id: 99001,
            subject: "audio dropped".into(),
            description: "initial intake".into(),
            requester_org: None,
            requester_email: None,
            tags: vec![],
            created_at: chrono::Utc::now(),
            updated_at: None,
            comments: vec![],
        };
        let base_evidence = crate::models::BaseEvidenceManifest {
            schema: "triage-cli/base-evidence".into(),
            schema_version: 1,
            ticket_id: "99001".into(),
            captured_at: chrono::Utc::now(),
            evidence: vec![
                crate::models::EvidenceItem {
                    id: "E-001".into(),
                    kind: "datadog_log_window".into(),
                    label: "JeffCom 2026-05-13T07:00 to 07:30".into(),
                    source_time: None,
                    source_path: "datadog:log_window".into(),
                },
                crate::models::EvidenceItem {
                    id: "E-002".into(),
                    kind: "local_file".into(),
                    label: "apex.log".into(),
                    source_time: None,
                    source_path: "local:apex.log".into(),
                },
            ],
        };
        let post_base_turn = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "99001".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: chrono::Utc::now(),
            author: Some("enrique".into()),
            body: "follow-up question".into(),
            evidence: vec![crate::models::EvidenceProvenance::Paste {
                label: "new-paste".into(),
                body: "NEW_EVIDENCE_SENTINEL".into(),
                bytes: 21,
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
        };

        let session = build_revise_session(&base_ticket, &base_evidence, &[post_base_turn], 0);

        let pasted_texts: Vec<&str> = session
            .evidence
            .pasted_logs
            .iter()
            .map(|p| p.text.as_str())
            .collect();
        // Base-evidence catalog must be surfaced (E-001 and E-002 ids preserved).
        let catalog_text = pasted_texts
            .iter()
            .find(|s| s.contains("E-001") && s.contains("E-002"))
            .copied()
            .unwrap_or_else(|| {
                panic!("base-evidence catalog missing; pasted_logs = {pasted_texts:?}")
            });
        assert!(
            catalog_text.contains("datadog_log_window") && catalog_text.contains("apex.log"),
            "catalog summary does not name original evidence kinds/labels: {catalog_text}"
        );
        // Post-base evidence must also be in the session.
        assert!(
            pasted_texts
                .iter()
                .any(|s| s.contains("NEW_EVIDENCE_SENTINEL")),
            "post-base evidence missing; pasted_logs = {pasted_texts:?}"
        );
    }
}
