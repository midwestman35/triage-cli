//! End-to-end pipeline that turns a fetched ticket into a five-markdown
//! ticket folder (spec § 4). The `Reporter` trait decouples progress output
//! from orchestration: `StderrReporter` (default), `SilentReporter`
//! (tests/watcher), `ChannelReporter` (TUI).

use std::collections::HashMap;
use std::path::PathBuf;
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
        let sites_path_buf = crate::paths::triage_home().join("data/cnc-map.json");
        let sites_path = sites_path_buf.as_path();
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
            schema_version: 2,
            ticket_id: ticket.id.to_string(),
            captured_at: chrono::Utc::now(),
            evidence: collect_base_evidence_entries(&bundle),
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

/// Per-entry cap on base-evidence body snapshots. Kept in sync with
/// ZIP_ENTRY_CAP_BYTES in investigation.rs so attached zip entries and
/// snapshot bodies share the same size budget.
const BODY_SNAPSHOT_CAP_BYTES: usize = 256 * 1024;

/// Truncate `body` to at most `BODY_SNAPSHOT_CAP_BYTES`, respecting UTF-8
/// char boundaries. Appends a `"\n\n[truncated]"` marker when truncation
/// occurs — the returned string may exceed the cap by ~14 bytes (the
/// marker length). Returns `None` for empty input (including the
/// pathological case where the entire cap is one codepoint).
fn cap_body_snapshot(body: String) -> Option<String> {
    if body.is_empty() {
        return None;
    }
    if body.len() <= BODY_SNAPSHOT_CAP_BYTES {
        return Some(body);
    }
    // Find the last char boundary at or below the cap so we never slice
    // mid-codepoint.
    let mut cut = BODY_SNAPSHOT_CAP_BYTES;
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    // Pathological case: the entire cap window is occupied by a single
    // multi-byte codepoint — no valid boundary found. Return None rather
    // than Some("\n\n[truncated]"), which would violate the contract.
    if cut == 0 {
        return None;
    }
    let mut truncated = body[..cut].to_string();
    truncated.push_str("\n\n[truncated]");
    Some(truncated)
}

/// Build `BaseEvidenceEntry` list from the catalog (`bundle.evidence_index`)
/// plus the bundle's content fields. Populates `body` per kind; returns
/// `None` for kinds that can't be matched or that yield an empty body.
///
/// Extracted as a free function so the mapping is unit-testable in
/// isolation from the rest of `investigate_one_structured`.
fn collect_base_evidence_entries(bundle: &TriageBundle) -> Vec<crate::models::BaseEvidenceEntry> {
    use crate::models::BaseEvidenceEntry;
    bundle
        .evidence_index
        .iter()
        .map(|item| {
            let body = match item.kind.as_str() {
                // Note: matches by label only. If multiple pasted_logs share the
                // same label, only the first match's body is captured — a
                // pre-existing ambiguity in assign_evidence_ids. Tracked in
                // ADR-0003.
                "pasted_note" => bundle
                    .pasted_logs
                    .iter()
                    .find(|p| p.label == item.label)
                    .and_then(|p| cap_body_snapshot(p.text.clone())),
                // Note: matches by basename only. Two local files with the same
                // basename in different directories collide on the first match.
                // Same pre-existing ambiguity as the pasted_note arm.
                "local_file" => bundle
                    .local_files
                    .iter()
                    .find(|lf| {
                        lf.path
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| lf.path.display().to_string())
                            == item.label
                    })
                    .and_then(|lf| lf.extracted_text.clone())
                    .and_then(cap_body_snapshot),
                "datadog_log_window" => {
                    if bundle.log_lines.is_empty() {
                        None
                    } else {
                        let rendered = bundle
                            .log_lines
                            .iter()
                            .map(|l| {
                                format!(
                                    "{} [{}] {}",
                                    crate::models::fmt_ts(&l.timestamp),
                                    l.level,
                                    l.message
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        cap_body_snapshot(rendered)
                    }
                }
                "zendesk_comment" => bundle
                    .ticket
                    .comments
                    .iter()
                    .find(|c| {
                        format!("comment:{}", crate::models::fmt_ts(&c.created_at))
                            == item.source_path
                    })
                    .and_then(|c| cap_body_snapshot(c.body.clone())),
                "attachment" => bundle
                    .downloaded_attachments
                    .iter()
                    .find(|a| a.filename == item.label)
                    .and_then(|a| a.extracted_text.clone())
                    .and_then(cap_body_snapshot),
                "customer_history" => bundle.customer_history.as_ref().and_then(|h| {
                    if h.tickets.is_empty() {
                        return None;
                    }
                    let mut lines = vec![format!("{} prior ticket(s):", h.tickets.len())];
                    for t in &h.tickets {
                        lines.push(format!(
                            "- #{} [{}] {} (created {})",
                            t.id,
                            t.status,
                            t.subject,
                            crate::models::fmt_ts(&t.created_at)
                        ));
                    }
                    cap_body_snapshot(lines.join("\n"))
                }),
                "memory_hit" => bundle.memory_context.as_ref().and_then(|ctx| {
                    let needle = item
                        .source_path
                        .strip_prefix("memory:")
                        .unwrap_or(&item.source_path);
                    ctx.entries
                        .iter()
                        .find(|e| e.ticket_id == needle)
                        .and_then(|e| {
                            cap_body_snapshot(format!(
                                "ticket_id: {}\nsubject: {}\nassessment: {}",
                                e.ticket_id, e.subject, e.assessment
                            ))
                        })
                }),
                _ => None,
            };
            BaseEvidenceEntry {
                item: item.clone(),
                body,
            }
        })
        .collect()
}

/// The current analyst's identifier for `STATE.md`. Falls back through
/// `TRIAGE_OWNER` → `USER` (unix) → `USERNAME` (Windows) → "unknown" so the
/// soft-lock has a useful value even in headless / CI environments and on
/// Windows where `$USER` does not exist.
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
    if let Ok(v) = std::env::var("USERNAME") {
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

/// Action value written to the System turn that signals a codex session was
/// lost (provider failed to resume the prior session and started fresh).
/// Extracted to a const so implementation and tests stay in sync.
const SESSION_LOST_ACTION: &str = "session_lost";

/// Append a follow-up turn pair (analyst question + provider response)
/// to the conversation log under `ticket_dir`. Does NOT mutate the
/// five-markdown folder — that only happens on /revise (see
/// `investigate_one_structured` with `followup_mode=true`, Task 12).
///
/// Acquires the per-ticket lock for both writes (analyst turn + provider
/// turn). The caller is expected to have already validated `prompt` (e.g.
/// rendered it from analyst input + attached evidence bodies).
#[allow(clippy::too_many_arguments)]
pub async fn followup_turn(
    ticket_dir: &std::path::Path,
    ticket_id: &str,
    prompt: &str,
    system_prompt: &str,
    model: &str,
    attachments: &[crate::models::Attachment],
    provider: &dyn crate::providers::LlmProvider,
    reporter: Option<&dyn crate::chat::ChatPhaseReporter>,
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
    // Filter to Codex turns only, then take the most recent one's session_id.
    // Scanning all turns would pick up a stale session_id from an older codex
    // turn when the newest codex turn has session_id: None — that stale id
    // would trigger a spurious session_lost on the next followup.
    let last_codex_session = outcome
        .turns
        .iter()
        .rev()
        .find(|t| matches!(t.turn_kind, crate::models::TurnKind::Codex))
        .and_then(|t| t.session_id.clone());
    let next_turn = outcome.turns.iter().map(|t| t.turn).max().unwrap_or(0) + 1;
    let provider_is_codex = provider.name() == "codex";

    // Apply PII redaction at the LLM boundary (spec § 7.1g, § 9.3).
    // The redactor scrubs caller PII (phones, addresses, GPS coords);
    // operational identifiers (Call-IDs, station codes, CNC UUIDs,
    // site names) are preserved.
    let (redacted_prompt, _redaction_counts) = crate::redact::redact(prompt);

    // Seed ticket context into the system prompt (#22). Without this the
    // default Unleash provider (stateless HTTP) and the first Codex turn
    // answered with zero knowledge of the ticket or the fork decision. The
    // helper is internally PII-redacted and length-capped.
    //
    // When a prior Codex session exists, a resume is about to be attempted;
    // if it fails ("no rollout found for thread id") the codex provider
    // silently restarts a fresh `codex exec` with no server-side history
    // (#23). To make that fallback context-aware we additionally fold a
    // bounded replay of recent turns into the system prompt. Codex prepends
    // the system prompt on *both* the resume and the fresh-exec path, so
    // seeding it here covers the session-loss case without a signature
    // change. (The analyst-facing System warning turn is still appended
    // below — that behavior is unchanged.)
    let combined_system_prompt = {
        let mut parts: Vec<String> = Vec::new();
        // Redact caller system_prompt at the LLM boundary: `followup_turn` is
        // `pub`, so any future non-empty caller string must be scrubbed
        // regardless of caller convention.
        if !system_prompt.trim().is_empty() {
            let (redacted_sys, _) = crate::redact::redact(system_prompt);
            parts.push(redacted_sys);
        }
        if let Some(ctx) = chat::build_ticket_context_preamble(ticket_dir) {
            parts.push(ctx);
        }
        if last_codex_session.is_some() {
            if let Some(replay) =
                chat::build_conversation_replay(&outcome.turns, chat::CONVERSATION_REPLAY_TURNS)
            {
                parts.push(replay);
            }
        }
        // Apply an outer cap on the fully assembled prompt so that the
        // preamble + replay + caller string cannot exceed the combined ceiling
        // even when all three components are at their individual limits.
        let assembled = parts.join("\n\n");
        chat::truncate_on_boundary(
            &assembled,
            chat::COMBINED_SYSTEM_PROMPT_CAP_BYTES,
            "\n\n[system prompt truncated]",
        )
    };

    if let Some(reporter) = reporter {
        reporter.phase(crate::chat::ChatStage::ContextAssembled);
    }

    // Call provider
    if let Some(reporter) = reporter {
        if provider_is_codex && last_codex_session.is_some() {
            reporter.phase(crate::chat::ChatStage::SessionResumeAttempt);
        }
        reporter.phase(crate::chat::ChatStage::ProviderAwait);
    }
    let started = std::time::Instant::now();
    let result = provider
        .followup(
            last_codex_session.as_deref(),
            &redacted_prompt,
            &combined_system_prompt,
            model,
            attachments,
        )
        .await
        .map_err(FollowupError::Provider)?;
    let elapsed_s = started.elapsed().as_secs_f64();

    if let Some(reporter) = reporter {
        reporter.phase(crate::chat::ChatStage::ResponseParsed);
    }

    // Detect codex session-lost fallback: we attempted a resume (prior session
    // existed) but the provider did NOT resume — it started fresh without the
    // prior turn context. Insert a System turn BEFORE the codex turn so the
    // analyst knows the model has amnesia and can restate relevant facts.
    let session_lost = last_codex_session.is_some() && !result.resumed;
    let codex_turn_number = if session_lost {
        let system_turn = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: ticket_id.to_string(),
            turn: next_turn,
            turn_kind: crate::models::TurnKind::System,
            ts: chrono::Utc::now(),
            author: None,
            body: "Codex resume failed — continuing in a fresh session. Prior turn context is no longer available to the model; restate relevant facts in your next question if needed.".to_string(),
            evidence: vec![],
            provider: None,
            model: None,
            tokens_in: None,
            tokens_out: None,
            elapsed_s: None,
            session_id: None,
            resumed: None,
            action: Some(SESSION_LOST_ACTION.to_string()),
            outcome: None,
            drove_revision_from_turns: None,
            diff: None,
        };
        chat::append_turn(&conv, &system_turn).map_err(FollowupError::Chat)?;
        next_turn + 1
    } else {
        next_turn
    };

    // Append the provider turn
    let provider_turn = crate::models::Turn {
        schema: "triage-cli/conversation".into(),
        schema_version: 1,
        ticket_id: ticket_id.to_string(),
        turn: codex_turn_number,
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

    if let Some(reporter) = reporter {
        reporter.phase(crate::chat::ChatStage::Saved);
    }

    // Update manifest (best-effort — failure here is logged but not fatal)
    if let Ok(Some(mut m)) = chat::read_session_manifest_opt(ticket_dir) {
        // Only count a real resume: session-lost fallbacks issue a fresh
        // session_id but resumed=false, so gating on session_id.is_some()
        // would overcount. result.resumed is the unambiguous signal.
        if result.resumed {
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
/// `investigate_one_structured`. Seeds the session with the base-evidence
/// catalog (so the LLM re-emission preserves the original `E-NNN`
/// identifiers and labels) AND, for entries where the v2 manifest carries
/// a `body` snapshot, injects the captured body as a labeled paste so the
/// LLM re-emission sees the same signal that drove the original fork.
/// Then layers post-base evidence from analyst/automated turns recorded
/// since `last_revise_turn`.
///
/// **Schema v1 backward compatibility:** legacy manifests deserialize
/// into v2 with `entry.body == None`; those entries surface only in the
/// catalog summary, matching the pre-ADR-0003 behavior.
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
        let mut catalog = String::from("Base-evidence catalog from the original investigation:\n");
        for entry in &base_evidence.evidence {
            catalog.push_str(&format!(
                "- {} [{}] {}\n",
                entry.item.id, entry.item.kind, entry.item.label
            ));
        }
        crate::investigation::add_pasted_evidence(&mut session, "base-evidence-catalog", &catalog);
    }

    // 2. For each entry that carries a body snapshot (v2 manifests), inject
    //    the body as a labeled paste so the LLM re-emission has the same
    //    raw signal the original investigation did. Entries without a body
    //    (legacy v1 manifests, or kinds where extraction wasn't possible)
    //    surface only via the catalog above.
    for entry in &base_evidence.evidence {
        if let Some(body) = &entry.body {
            let label = format!("base-{}-{}", entry.item.kind, entry.item.id);
            crate::investigation::add_pasted_evidence(&mut session, &label, body);
        }
    }

    // 3. Layer post-base evidence from turns since the last revise.
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
            None,
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
    async fn followup_turn_inserts_system_note_when_codex_session_lost() {
        use crate::chat;
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44999");
        std::fs::create_dir_all(&ticket_dir).unwrap();

        // Seed conversation with an analyst turn AND a codex turn that has a
        // session_id — this is what triggers the resume-attempt path.
        let analyst = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44999".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: chrono::Utc::now(),
            author: Some("enrique".into()),
            body: "first question".into(),
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
        let codex = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44999".into(),
            turn: 2,
            turn_kind: crate::models::TurnKind::Codex,
            ts: chrono::Utc::now(),
            author: None,
            body: "first answer".into(),
            evidence: vec![],
            provider: Some("codex".into()),
            model: Some("gpt-5.5".into()),
            tokens_in: None,
            tokens_out: None,
            elapsed_s: None,
            session_id: Some("01HOLD000000".into()),
            resumed: Some(false),
            action: None,
            outcome: None,
            drove_revision_from_turns: None,
            diff: None,
        };
        let conv = chat::conversation_jsonl_path(&ticket_dir);
        {
            let _g = chat::acquire_session_lock(
                &chat::session_dir(&ticket_dir),
                std::time::Duration::from_secs(1),
            )
            .unwrap();
            chat::append_turn(&conv, &analyst).unwrap();
            chat::append_turn(&conv, &codex).unwrap();
        }

        // FakeProvider that mimics the codex session-lost fallback: resumed=false
        // with a freshly-issued session_id.
        struct LostSessionProvider;
        impl crate::providers::LlmProvider for LostSessionProvider {
            fn name(&self) -> &'static str {
                "fake-lost"
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
                        text: "fresh response".into(),
                        tokens_in: None,
                        tokens_out: None,
                    })
                })
            }
            fn followup<'a>(
                &'a self,
                _session_id: Option<&'a str>,
                _prompt: &'a str,
                _sys: &'a str,
                _model: &'a str,
                _attachments: &'a [crate::models::Attachment],
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                crate::providers::FollowupResult,
                                crate::providers::ProviderError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async move {
                    Ok(crate::providers::FollowupResult {
                        text: "fresh response".into(),
                        tokens_in: None,
                        tokens_out: None,
                        session_id: Some("01HFRESH00000".into()),
                        resumed: false,
                    })
                })
            }
        }

        let provider: Box<dyn crate::providers::LlmProvider> = Box::new(LostSessionProvider);
        followup_turn(
            &ticket_dir,
            "44999",
            "follow-up after session lost",
            "system",
            "gpt-5.5",
            &[],
            provider.as_ref(),
            None,
        )
        .await
        .unwrap();

        let parsed = chat::parse_conversation_jsonl(&conv).unwrap();
        // Expect: analyst(1), codex(2), system-session-lost(3), codex(4)
        assert_eq!(
            parsed.turns.len(),
            4,
            "expected analyst+codex+system+codex; got {:?}",
            parsed
                .turns
                .iter()
                .map(|t| (t.turn, &t.turn_kind, t.action.clone()))
                .collect::<Vec<_>>()
        );
        let system_turn = parsed
            .turns
            .iter()
            .find(|t| matches!(t.turn_kind, crate::models::TurnKind::System))
            .expect("system turn must be present");
        assert_eq!(system_turn.action.as_deref(), Some(SESSION_LOST_ACTION));
        // The system turn must precede the new codex turn in the JSONL.
        let positions: Vec<(u32, &crate::models::TurnKind)> = parsed
            .turns
            .iter()
            .map(|t| (t.turn, &t.turn_kind))
            .collect();
        let sys_idx = positions
            .iter()
            .position(|(_, k)| matches!(k, crate::models::TurnKind::System))
            .unwrap();
        let new_codex_idx = positions
            .iter()
            .rposition(|(_, k)| matches!(k, crate::models::TurnKind::Codex))
            .unwrap();
        assert!(
            sys_idx < new_codex_idx,
            "system turn must precede new codex turn in JSONL order"
        );
    }

    #[tokio::test]
    async fn followup_turn_no_system_note_on_first_followup() {
        // No prior codex session: provider returns resumed=false with a fresh
        // session_id, which is normal first-followup behavior — NOT session-lost.
        // The function must NOT insert a system "session_lost" turn here.
        use crate::chat;
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44998");
        std::fs::create_dir_all(&ticket_dir).unwrap();

        // Seed only an analyst turn — no prior codex session_id exists.
        let analyst = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44998".into(),
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
        let conv = chat::conversation_jsonl_path(&ticket_dir);
        {
            let _g = chat::acquire_session_lock(
                &chat::session_dir(&ticket_dir),
                std::time::Duration::from_secs(1),
            )
            .unwrap();
            chat::append_turn(&conv, &analyst).unwrap();
        }

        // Provider returns resumed=false with a new session_id — normal first-call.
        struct FirstFollowupProvider;
        impl crate::providers::LlmProvider for FirstFollowupProvider {
            fn name(&self) -> &'static str {
                "fake-first"
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
                        text: "first codex reply".into(),
                        tokens_in: None,
                        tokens_out: None,
                    })
                })
            }
            fn followup<'a>(
                &'a self,
                _session_id: Option<&'a str>,
                _prompt: &'a str,
                _sys: &'a str,
                _model: &'a str,
                _attachments: &'a [crate::models::Attachment],
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                crate::providers::FollowupResult,
                                crate::providers::ProviderError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async move {
                    Ok(crate::providers::FollowupResult {
                        text: "first codex reply".into(),
                        tokens_in: None,
                        tokens_out: None,
                        session_id: Some("01HFIRST00000".into()),
                        resumed: false,
                    })
                })
            }
        }

        let provider: Box<dyn crate::providers::LlmProvider> = Box::new(FirstFollowupProvider);
        followup_turn(
            &ticket_dir,
            "44998",
            "first follow-up question",
            "system",
            "gpt-5.5",
            &[],
            provider.as_ref(),
            None,
        )
        .await
        .unwrap();

        let parsed = chat::parse_conversation_jsonl(&conv).unwrap();
        // Expect: analyst(1), codex(2) — no system turn
        assert_eq!(
            parsed.turns.len(),
            2,
            "expected analyst+codex only (no session_lost system turn); got {:?}",
            parsed
                .turns
                .iter()
                .map(|t| (t.turn, &t.turn_kind, t.action.clone()))
                .collect::<Vec<_>>()
        );
        assert!(
            !parsed
                .turns
                .iter()
                .any(|t| matches!(t.turn_kind, crate::models::TurnKind::System)),
            "no System turn expected on first follow-up"
        );
    }

    #[tokio::test]
    async fn followup_turn_no_system_note_when_latest_codex_session_id_is_none() {
        // Regression: when the most-recent codex turn has session_id=None,
        // last_codex_session must be None and NO resume is attempted. Because
        // no resume was attempted, the provider's resumed=false response must
        // NOT be interpreted as a session-lost event, and no System turn
        // should be inserted.
        //
        // Turn layout seeded:
        //   turn-1  Analyst
        //   turn-2  Codex  session_id="01HOLD000000"  (older; must be ignored)
        //   turn-3  Analyst
        //   turn-4  Codex  session_id=None             (most recent codex)
        //   turn-5  Analyst
        use crate::chat;
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44996");
        std::fs::create_dir_all(&ticket_dir).unwrap();

        let make_analyst = |turn: u32, body: &str| crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44996".into(),
            turn,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: chrono::Utc::now(),
            author: Some("enrique".into()),
            body: body.to_string(),
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
        let make_codex = |turn: u32, sid: Option<&str>| crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44996".into(),
            turn,
            turn_kind: crate::models::TurnKind::Codex,
            ts: chrono::Utc::now(),
            author: None,
            body: "codex answer".into(),
            evidence: vec![],
            provider: Some("codex".into()),
            model: Some("gpt-5.5".into()),
            tokens_in: None,
            tokens_out: None,
            elapsed_s: None,
            session_id: sid.map(str::to_string),
            resumed: Some(false),
            action: None,
            outcome: None,
            drove_revision_from_turns: None,
            diff: None,
        };

        let conv = chat::conversation_jsonl_path(&ticket_dir);
        {
            let _g = chat::acquire_session_lock(
                &chat::session_dir(&ticket_dir),
                std::time::Duration::from_secs(1),
            )
            .unwrap();
            chat::append_turn(&conv, &make_analyst(1, "question one")).unwrap();
            chat::append_turn(&conv, &make_codex(2, Some("01HOLD000000"))).unwrap();
            chat::append_turn(&conv, &make_analyst(3, "question two")).unwrap();
            chat::append_turn(&conv, &make_codex(4, None)).unwrap(); // most recent codex: no session_id
            chat::append_turn(&conv, &make_analyst(5, "question three")).unwrap();
        }

        // Provider always returns resumed=false; if last_codex_session were
        // mistakenly set to "01HOLD000000" this would be interpreted as
        // session-lost and a System turn would be inserted.
        struct ResumedFalseProvider;
        impl crate::providers::LlmProvider for ResumedFalseProvider {
            fn name(&self) -> &'static str {
                "fake-resumed-false"
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
                        text: "answer".into(),
                        tokens_in: None,
                        tokens_out: None,
                    })
                })
            }
            fn followup<'a>(
                &'a self,
                _session_id: Option<&'a str>,
                _prompt: &'a str,
                _sys: &'a str,
                _model: &'a str,
                _attachments: &'a [crate::models::Attachment],
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                crate::providers::FollowupResult,
                                crate::providers::ProviderError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async move {
                    Ok(crate::providers::FollowupResult {
                        text: "fresh answer".into(),
                        tokens_in: None,
                        tokens_out: None,
                        session_id: Some("01HNEW000001".into()),
                        resumed: false,
                    })
                })
            }
        }

        let provider: Box<dyn crate::providers::LlmProvider> = Box::new(ResumedFalseProvider);
        followup_turn(
            &ticket_dir,
            "44996",
            "follow-up after codex with no session_id",
            "system",
            "gpt-5.5",
            &[],
            provider.as_ref(),
            None,
        )
        .await
        .unwrap();

        let parsed = chat::parse_conversation_jsonl(&conv).unwrap();
        // Expect: analyst(1)+codex(2)+analyst(3)+codex(4)+analyst(5)+codex(6)
        // — no System turn because last_codex_session was None.
        assert!(
            !parsed
                .turns
                .iter()
                .any(|t| matches!(t.turn_kind, crate::models::TurnKind::System)),
            "no System turn expected when most recent codex had session_id=None; turns: {:?}",
            parsed
                .turns
                .iter()
                .map(|t| (t.turn, &t.turn_kind, t.action.as_deref()))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            parsed.turns.len(),
            6,
            "expected 6 turns (5 seeded + 1 new codex); got {:?}",
            parsed
                .turns
                .iter()
                .map(|t| (t.turn, &t.turn_kind))
                .collect::<Vec<_>>()
        );
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
            None,
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
    async fn followup_turn_seeds_ticket_context_into_system_prompt() {
        // #22: the chat path passes an empty system prompt; followup_turn
        // must rebuild ticket context (STATE.md / FORK_PACKET.md) and feed
        // it to the provider so Unleash (stateless) and the first Codex
        // turn are not context-blind.
        use std::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44778");
        std::fs::create_dir_all(&ticket_dir).unwrap();
        std::fs::write(
            ticket_dir.join("STATE.md"),
            "---\nticket_id: 44778\nfork: A\n---\n",
        )
        .unwrap();
        std::fs::write(
            ticket_dir.join("FORK_PACKET.md"),
            "Recommendation: Fork A — FORK_CONTEXT_SENTINEL.\n",
        )
        .unwrap();

        // Seed an analyst turn-001 so the conversation file exists.
        let analyst = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44778".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: chrono::Utc::now(),
            author: Some("enrique".into()),
            body: "does the fork still hold?".into(),
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

        struct SysCapturingProvider {
            captured_system: Mutex<Option<String>>,
        }
        impl crate::providers::LlmProvider for SysCapturingProvider {
            fn name(&self) -> &'static str {
                "fake-sys-capturing"
            }
            fn complete<'a>(
                &'a self,
                _prompt: &'a str,
                system: &'a str,
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
                    *self.captured_system.lock().unwrap() = Some(system.to_string());
                    Ok(crate::providers::CompletionResult {
                        text: "context-seed reply".into(),
                        tokens_in: Some(5),
                        tokens_out: Some(5),
                    })
                })
            }
        }

        let provider = SysCapturingProvider {
            captured_system: Mutex::new(None),
        };
        // Caller passes an empty system prompt, exactly like the inbox.
        followup_turn(
            &ticket_dir,
            "44778",
            "does the fork still hold?",
            "",
            "fake-model",
            &[],
            &provider,
            None,
        )
        .await
        .unwrap();

        let captured = provider
            .captured_system
            .lock()
            .unwrap()
            .clone()
            .expect("provider must have been called");
        assert!(
            captured.contains("FORK_CONTEXT_SENTINEL"),
            "ticket context was not seeded into system prompt; got: {captured:?}"
        );
        assert!(
            captured.contains("STATE.md") && captured.contains("fork: A"),
            "STATE.md context missing from system prompt; got: {captured:?}"
        );
        // No prior Codex session → no replay block on this first turn.
        assert!(
            !captured.contains("Prior conversation (replayed"),
            "unexpected replay block on first turn: {captured:?}"
        );
    }

    // ── Issue 1: caller system_prompt redaction ───────────────────────────

    #[tokio::test]
    async fn followup_turn_redacts_caller_system_prompt() {
        // Verifies that PII in the caller-supplied system_prompt is scrubbed
        // before reaching the provider. The production caller (inbox TUI)
        // passes "", but followup_turn is pub, so the invariant must hold
        // structurally (issue raised in pre-merge review).
        use std::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44900");
        std::fs::create_dir_all(&ticket_dir).unwrap();

        // Seed one analyst turn so the conversation file exists.
        let analyst = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44900".into(),
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

        struct SysCapture {
            captured_sys: Mutex<Option<String>>,
        }
        impl crate::providers::LlmProvider for SysCapture {
            fn name(&self) -> &'static str {
                "fake-sys-capture"
            }
            fn complete<'a>(
                &'a self,
                _prompt: &'a str,
                system: &'a str,
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
                    *self.captured_sys.lock().unwrap() = Some(system.to_string());
                    Ok(crate::providers::CompletionResult {
                        text: "sys-redact reply".into(),
                        tokens_in: None,
                        tokens_out: None,
                    })
                })
            }
        }

        let provider = SysCapture {
            captured_sys: Mutex::new(None),
        };

        // Caller passes a non-empty system_prompt containing a phone number
        // (same pattern as redact::tests::redacts_phone).
        let sys_with_pii = "analyst hotline: (555) 123-4567 — do not share";
        followup_turn(
            &ticket_dir,
            "44900",
            "what is the next step?",
            sys_with_pii,
            "fake-model",
            &[],
            &provider,
            None,
        )
        .await
        .unwrap();

        let captured = provider
            .captured_sys
            .lock()
            .unwrap()
            .clone()
            .expect("provider must have been called");

        // The raw phone MUST NOT appear in the system prompt the provider saw.
        assert!(
            !captured.contains("123-4567"),
            "caller system_prompt PII leaked to provider; got: {captured:?}"
        );
        // The redaction sentinel MUST be present.
        assert!(
            captured.contains("<PHONE>"),
            "expected <PHONE> sentinel in redacted system prompt; got: {captured:?}"
        );
    }

    // ── Issue 2: outer cap on combined system prompt ──────────────────────

    #[tokio::test]
    async fn followup_turn_combined_system_prompt_is_capped() {
        // Verifies that when all three components (caller prompt + preamble +
        // replay) stack up, the assembled combined_system_prompt is truncated
        // to COMBINED_SYSTEM_PROMPT_CAP_BYTES on a UTF-8 char boundary.
        use crate::chat;
        use std::sync::Mutex;

        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44901");
        std::fs::create_dir_all(&ticket_dir).unwrap();

        // Write STATE.md and FORK_PACKET.md each much larger than the preamble
        // cap to ensure the preamble component reaches its individual ceiling.
        let fat_content = "X".repeat(chat::CONTEXT_PREAMBLE_CAP_BYTES * 3);
        std::fs::write(ticket_dir.join("STATE.md"), &fat_content).unwrap();
        std::fs::write(ticket_dir.join("FORK_PACKET.md"), &fat_content).unwrap();

        // Seed a prior Codex turn with a session_id to trigger the replay path,
        // and make its body large enough to fill the replay component.
        let analyst = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44901".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: chrono::Utc::now(),
            author: None,
            body: "Y".repeat(chat::CONTEXT_PREAMBLE_CAP_BYTES),
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
        let codex = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "44901".into(),
            turn: 2,
            turn_kind: crate::models::TurnKind::Codex,
            ts: chrono::Utc::now(),
            author: None,
            body: "Z".repeat(chat::CONTEXT_PREAMBLE_CAP_BYTES),
            evidence: vec![],
            provider: Some("codex".into()),
            model: None,
            tokens_in: None,
            tokens_out: None,
            elapsed_s: None,
            session_id: Some("01HBIG000001".into()),
            resumed: Some(false),
            action: None,
            outcome: None,
            drove_revision_from_turns: None,
            diff: None,
        };
        let conv = chat::conversation_jsonl_path(&ticket_dir);
        {
            let _guard = chat::acquire_session_lock(
                &chat::session_dir(&ticket_dir),
                std::time::Duration::from_secs(1),
            )
            .unwrap();
            chat::append_turn(&conv, &analyst).unwrap();
            chat::append_turn(&conv, &codex).unwrap();
        }

        struct SysCap {
            captured_sys: Mutex<Option<String>>,
        }
        impl crate::providers::LlmProvider for SysCap {
            fn name(&self) -> &'static str {
                "fake-sys-cap"
            }
            fn complete<'a>(
                &'a self,
                _p: &'a str,
                sys: &'a str,
                _m: &'a str,
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
                    *self.captured_sys.lock().unwrap() = Some(sys.to_string());
                    Ok(crate::providers::CompletionResult {
                        text: "cap reply".into(),
                        tokens_in: None,
                        tokens_out: None,
                    })
                })
            }
            fn followup<'a>(
                &'a self,
                _session_id: Option<&'a str>,
                _prompt: &'a str,
                sys: &'a str,
                _model: &'a str,
                _attachments: &'a [crate::models::Attachment],
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                crate::providers::FollowupResult,
                                crate::providers::ProviderError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async move {
                    *self.captured_sys.lock().unwrap() = Some(sys.to_string());
                    Ok(crate::providers::FollowupResult {
                        text: "cap reply".into(),
                        tokens_in: None,
                        tokens_out: None,
                        session_id: None,
                        resumed: false,
                    })
                })
            }
        }

        let provider = SysCap {
            captured_sys: Mutex::new(None),
        };

        // Caller prompt is also oversized to stress the combined ceiling.
        // Use the combined cap as the caller prompt size so that caller alone
        // already fills the ceiling; together with even a small preamble it
        // will reliably exceed COMBINED_SYSTEM_PROMPT_CAP_BYTES and trigger
        // the outer truncation.
        let big_caller_sys = "A".repeat(chat::COMBINED_SYSTEM_PROMPT_CAP_BYTES);
        followup_turn(
            &ticket_dir,
            "44901",
            "question",
            &big_caller_sys,
            "fake-model",
            &[],
            &provider,
            None,
        )
        .await
        .unwrap();

        let captured = provider
            .captured_sys
            .lock()
            .unwrap()
            .clone()
            .expect("provider must have been called");

        // The combined system prompt must not exceed the outer cap plus the
        // truncation marker length (allow a small slack for the marker).
        assert!(
            captured.len() <= chat::COMBINED_SYSTEM_PROMPT_CAP_BYTES + 64,
            "combined system prompt exceeded cap: {} bytes (cap={}, slack=64); \
            first 120 bytes: {:?}",
            captured.len(),
            chat::COMBINED_SYSTEM_PROMPT_CAP_BYTES,
            &captured[..captured.len().min(120)],
        );
        // Truncation marker must be present (confirms truncation ran).
        assert!(
            captured.contains("[system prompt truncated]"),
            "expected '[system prompt truncated]' in capped system prompt; got: {captured:?}"
        );
    }

    #[derive(Default)]
    struct RecordingChatReporter {
        stages: std::sync::Mutex<Vec<crate::chat::ChatStage>>,
    }

    impl crate::chat::ChatPhaseReporter for RecordingChatReporter {
        fn phase(&self, stage: crate::chat::ChatStage) {
            self.stages.lock().unwrap().push(stage);
        }
    }

    #[tokio::test]
    async fn followup_turn_emits_phases_in_order_with_codex_resume() {
        use crate::chat;

        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("45001");
        std::fs::create_dir_all(chat::session_dir(&ticket_dir)).unwrap();

        let prior = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "45001".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Codex,
            ts: chrono::Utc::now(),
            author: None,
            body: "prior".into(),
            evidence: vec![],
            provider: Some("codex".into()),
            model: Some("gpt-5.5".into()),
            tokens_in: None,
            tokens_out: None,
            elapsed_s: None,
            session_id: Some("01HPRIOR".into()),
            resumed: Some(false),
            action: None,
            outcome: None,
            drove_revision_from_turns: None,
            diff: None,
        };
        chat::append_turn(&chat::conversation_jsonl_path(&ticket_dir), &prior).unwrap();

        struct ResumeProvider;
        impl crate::providers::LlmProvider for ResumeProvider {
            fn name(&self) -> &'static str {
                "codex"
            }

            fn complete<'a>(
                &'a self,
                _prompt: &'a str,
                _system_prompt: &'a str,
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
                Box::pin(async { unreachable!("followup override is used") })
            }

            fn followup<'a>(
                &'a self,
                _session_id: Option<&'a str>,
                _prompt: &'a str,
                _system_prompt: &'a str,
                _model: &'a str,
                _attachments: &'a [crate::models::Attachment],
            ) -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                crate::providers::FollowupResult,
                                crate::providers::ProviderError,
                            >,
                        > + Send
                        + 'a,
                >,
            > {
                Box::pin(async {
                    Ok(crate::providers::FollowupResult {
                        text: "ok".into(),
                        tokens_in: None,
                        tokens_out: None,
                        session_id: Some("01HNEW".into()),
                        resumed: true,
                    })
                })
            }
        }

        let reporter = RecordingChatReporter::default();
        followup_turn(
            &ticket_dir,
            "45001",
            "what next?",
            "",
            "gpt-5.5",
            &[],
            &ResumeProvider,
            Some(&reporter),
        )
        .await
        .unwrap();

        assert_eq!(
            reporter.stages.lock().unwrap().as_slice(),
            &[
                crate::chat::ChatStage::ContextAssembled,
                crate::chat::ChatStage::SessionResumeAttempt,
                crate::chat::ChatStage::ProviderAwait,
                crate::chat::ChatStage::ResponseParsed,
                crate::chat::ChatStage::Saved,
            ]
        );
    }

    #[tokio::test]
    async fn followup_turn_skips_resume_phase_when_no_prior_session() {
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("45002");
        std::fs::create_dir_all(crate::chat::session_dir(&ticket_dir)).unwrap();

        struct FirstProvider;
        impl crate::providers::LlmProvider for FirstProvider {
            fn name(&self) -> &'static str {
                "fake-first"
            }

            fn complete<'a>(
                &'a self,
                _prompt: &'a str,
                _system_prompt: &'a str,
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
                Box::pin(async {
                    Ok(crate::providers::CompletionResult {
                        text: "ok".into(),
                        tokens_in: None,
                        tokens_out: None,
                    })
                })
            }
        }

        let reporter = RecordingChatReporter::default();
        followup_turn(
            &ticket_dir,
            "45002",
            "first question",
            "",
            "gpt-5.5",
            &[],
            &FirstProvider,
            Some(&reporter),
        )
        .await
        .unwrap();

        assert_eq!(
            reporter.stages.lock().unwrap().as_slice(),
            &[
                crate::chat::ChatStage::ContextAssembled,
                crate::chat::ChatStage::ProviderAwait,
                crate::chat::ChatStage::ResponseParsed,
                crate::chat::ChatStage::Saved,
            ]
        );
    }

    #[tokio::test]
    async fn followup_turn_skips_resume_phase_for_non_codex_provider_with_prior_session() {
        use crate::chat;

        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("45003");
        std::fs::create_dir_all(chat::session_dir(&ticket_dir)).unwrap();

        let prior = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "45003".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Codex,
            ts: chrono::Utc::now(),
            author: None,
            body: "prior".into(),
            evidence: vec![],
            provider: Some("codex".into()),
            model: Some("gpt-5.5".into()),
            tokens_in: None,
            tokens_out: None,
            elapsed_s: None,
            session_id: Some("01HPRIOR".into()),
            resumed: Some(false),
            action: None,
            outcome: None,
            drove_revision_from_turns: None,
            diff: None,
        };
        chat::append_turn(&chat::conversation_jsonl_path(&ticket_dir), &prior).unwrap();

        struct UnleashLikeProvider;
        impl crate::providers::LlmProvider for UnleashLikeProvider {
            fn name(&self) -> &'static str {
                "unleash"
            }

            fn complete<'a>(
                &'a self,
                _prompt: &'a str,
                _system_prompt: &'a str,
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
                Box::pin(async {
                    Ok(crate::providers::CompletionResult {
                        text: "ok".into(),
                        tokens_in: None,
                        tokens_out: None,
                    })
                })
            }
        }

        let reporter = RecordingChatReporter::default();
        followup_turn(
            &ticket_dir,
            "45003",
            "what next?",
            "",
            "gpt-5.5",
            &[],
            &UnleashLikeProvider,
            Some(&reporter),
        )
        .await
        .unwrap();

        assert_eq!(
            reporter.stages.lock().unwrap().as_slice(),
            &[
                crate::chat::ChatStage::ContextAssembled,
                crate::chat::ChatStage::ProviderAwait,
                crate::chat::ChatStage::ResponseParsed,
                crate::chat::ChatStage::Saved,
            ]
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
            schema_version: 2,
            ticket_id: "99001".into(),
            captured_at: chrono::Utc::now(),
            evidence: vec![
                crate::models::BaseEvidenceEntry {
                    item: crate::models::EvidenceItem {
                        id: "E-001".into(),
                        kind: "datadog_log_window".into(),
                        label: "JeffCom 2026-05-13T07:00 to 07:30".into(),
                        source_time: None,
                        source_path: "datadog:log_window".into(),
                    },
                    body: None,
                },
                crate::models::BaseEvidenceEntry {
                    item: crate::models::EvidenceItem {
                        id: "E-002".into(),
                        kind: "local_file".into(),
                        label: "apex.log".into(),
                        source_time: None,
                        source_path: "local:apex.log".into(),
                    },
                    body: None,
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

    #[test]
    fn collect_base_evidence_entries_copies_paste_body() {
        // collect_base_evidence_entries must copy a pasted_note's text into
        // the entry's `body` field — the central guarantee of ADR-0003 for
        // the pasted_note kind.
        use chrono::TimeZone;
        let ticket = crate::models::Ticket {
            id: 1,
            subject: "t".into(),
            description: "d".into(),
            requester_org: None,
            requester_email: None,
            tags: vec![],
            created_at: chrono::Utc.with_ymd_and_hms(2026, 5, 12, 0, 0, 0).unwrap(),
            updated_at: None,
            comments: vec![],
        };
        let mut bundle = crate::models::TriageBundle {
            ticket,
            site_entry: None,
            log_lines: vec![],
            log_truncated: false,
            anchor: None,
            anchor_source: None,
            window_start: None,
            window_end: None,
            downloaded_attachments: vec![],
            local_files: vec![],
            pasted_logs: vec![crate::models::PastedEvidence {
                label: "customer-note".into(),
                text: "PASTE_BODY_SENTINEL_42".into(),
            }],
            customer_history: None,
            memory_context: None,
            evidence_index: vec![],
        };
        bundle.evidence_index = crate::models::assign_evidence_ids(&bundle);
        // Assign deterministic IDs (E-NNN) as the production pipeline does.
        // The lookup in `collect_base_evidence_entries` is by label, not id,
        // but we still set ids for realism.
        for (counter, it) in (1..).zip(bundle.evidence_index.iter_mut()) {
            it.id = format!("E-{counter:03}");
        }

        let entries = collect_base_evidence_entries(&bundle);
        let paste_entry = entries
            .iter()
            .find(|e| e.item.kind == "pasted_note")
            .expect("pasted_note entry missing");
        assert_eq!(
            paste_entry.body.as_deref(),
            Some("PASTE_BODY_SENTINEL_42"),
            "pasted_note body was not captured into BaseEvidenceEntry"
        );
    }

    #[test]
    fn base_evidence_legacy_v1_manifest_parses_with_none_bodies() {
        // Old v1 manifests on disk have no `body` field per entry. They must
        // deserialize cleanly into the v2 BaseEvidenceEntry shape, with
        // `body == None` everywhere (serde flatten + default).
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join(".session");
        std::fs::create_dir_all(&session_dir).unwrap();
        let manifest_path = session_dir.join("base-evidence-manifest.json");
        // Hand-rolled v1 JSON: evidence entries are flat EvidenceItem
        // objects with no `body` field.
        let v1_json = r#"{
            "schema": "triage-cli/base-evidence",
            "schema_version": 1,
            "ticket_id": "12345",
            "captured_at": "2026-05-12T00:00:00Z",
            "evidence": [
                {
                    "id": "E-001",
                    "kind": "datadog_log_window",
                    "label": "site log window",
                    "source_path": "datadog:log_window"
                },
                {
                    "id": "E-002",
                    "kind": "local_file",
                    "label": "apex.log",
                    "source_path": "local:apex.log"
                }
            ]
        }"#;
        std::fs::write(&manifest_path, v1_json).unwrap();
        let bem =
            crate::chat::read_base_evidence_manifest(dir.path()).expect("v1 manifest must parse");
        assert_eq!(bem.evidence.len(), 2);
        for entry in &bem.evidence {
            assert!(
                entry.body.is_none(),
                "v1 entry {} unexpectedly carries a body: {:?}",
                entry.item.id,
                entry.body
            );
        }
    }

    #[test]
    fn build_revise_session_injects_body_snapshots() {
        // /revise must inject each non-None body snapshot from the v2
        // manifest as a labeled paste in the synthetic session, so the LLM
        // re-emission sees the raw signal that drove the original fork
        // (not just the E-NNN catalog).
        let base_ticket = crate::models::Ticket {
            id: 99002,
            subject: "audio stutter".into(),
            description: "".into(),
            requester_org: None,
            requester_email: None,
            tags: vec![],
            created_at: chrono::Utc::now(),
            updated_at: None,
            comments: vec![],
        };
        let base_evidence = crate::models::BaseEvidenceManifest {
            schema: "triage-cli/base-evidence".into(),
            schema_version: 2,
            ticket_id: "99002".into(),
            captured_at: chrono::Utc::now(),
            evidence: vec![
                crate::models::BaseEvidenceEntry {
                    item: crate::models::EvidenceItem {
                        id: "E-001".into(),
                        kind: "datadog_log_window".into(),
                        label: "site window".into(),
                        source_time: None,
                        source_path: "datadog:log_window".into(),
                    },
                    body: Some("DD_LOG_BODY_SENTINEL".into()),
                },
                crate::models::BaseEvidenceEntry {
                    item: crate::models::EvidenceItem {
                        id: "E-002".into(),
                        kind: "local_file".into(),
                        label: "apex.log".into(),
                        source_time: None,
                        source_path: "local:apex.log".into(),
                    },
                    body: Some("LOCAL_FILE_BODY_SENTINEL".into()),
                },
                crate::models::BaseEvidenceEntry {
                    item: crate::models::EvidenceItem {
                        id: "E-003".into(),
                        kind: "pasted_note".into(),
                        label: "customer-note".into(),
                        source_time: None,
                        source_path: "pasted:customer-note".into(),
                    },
                    // Legacy entry without a body should be silently
                    // dropped from the body-injection pass — only the
                    // catalog summary mentions it.
                    body: None,
                },
            ],
        };

        let session = build_revise_session(&base_ticket, &base_evidence, &[], 0);
        let pasted_texts: Vec<&str> = session
            .evidence
            .pasted_logs
            .iter()
            .map(|p| p.text.as_str())
            .collect();
        // 1 catalog paste + 2 body pastes (E-001, E-002); E-003 body is None
        // and must NOT produce an extra paste.
        assert_eq!(
            session.evidence.pasted_logs.len(),
            3,
            "expected 3 pasted_logs (1 catalog + 2 bodies); got {}; logs = {pasted_texts:?}",
            session.evidence.pasted_logs.len()
        );
        assert!(
            pasted_texts
                .iter()
                .any(|s| s.contains("DD_LOG_BODY_SENTINEL")),
            "datadog body was not injected; pasted_logs = {pasted_texts:?}"
        );
        assert!(
            pasted_texts
                .iter()
                .any(|s| s.contains("LOCAL_FILE_BODY_SENTINEL")),
            "local-file body was not injected; pasted_logs = {pasted_texts:?}"
        );
        // The catalog summary must still be present (with all three
        // E-NNN ids) even though E-003 has no body.
        let catalog = pasted_texts
            .iter()
            .find(|s| s.contains("E-001") && s.contains("E-002") && s.contains("E-003"))
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "base-evidence catalog missing or incomplete; pasted_logs = {pasted_texts:?}"
                )
            });
        assert!(catalog.contains("datadog_log_window"));
        assert!(catalog.contains("pasted_note"));
    }

    #[test]
    fn cap_body_snapshot_empty_returns_none() {
        assert!(cap_body_snapshot(String::new()).is_none());
    }

    #[test]
    fn cap_body_snapshot_short_returns_unchanged() {
        let s = "short content".to_string();
        let result = cap_body_snapshot(s.clone()).unwrap();
        assert_eq!(result, s);
    }

    #[test]
    fn cap_body_snapshot_long_truncates_with_marker() {
        let s = "a".repeat(BODY_SNAPSHOT_CAP_BYTES + 100);
        let result = cap_body_snapshot(s).unwrap();
        assert!(result.contains("[truncated]"));
        // Marker overage is documented; allow ~14 bytes slack.
        assert!(result.len() <= BODY_SNAPSHOT_CAP_BYTES + 32);
    }

    #[test]
    fn current_owner_falls_back_to_username_when_user_unset() {
        // Save and clear both vars so we test in a known state.
        let prev_user = std::env::var("USER").ok();
        let prev_username = std::env::var("USERNAME").ok();
        let prev_triage_owner = std::env::var("TRIAGE_OWNER").ok();

        std::env::remove_var("USER");
        std::env::remove_var("TRIAGE_OWNER");
        std::env::set_var("USERNAME", "alice");

        assert_eq!(current_owner(), "alice");

        // Restore.
        match prev_user {
            Some(v) => std::env::set_var("USER", v),
            None => std::env::remove_var("USER"),
        }
        match prev_username {
            Some(v) => std::env::set_var("USERNAME", v),
            None => std::env::remove_var("USERNAME"),
        }
        match prev_triage_owner {
            Some(v) => std::env::set_var("TRIAGE_OWNER", v),
            None => std::env::remove_var("TRIAGE_OWNER"),
        }
    }
}
