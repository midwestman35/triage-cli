use crate::models::{CustomerHistoryEvidence, MemoryEntry, StructuredTriageReport};
use crate::ticket_folder::TicketFolderPaths;

/// Options bag for `investigate_one_structured`. Avoids long parameter lists at call sites.
#[derive(Debug, Default, Clone)]
pub struct InvestigateOptions {
    pub interactive: bool,
    pub workspace: Option<std::path::PathBuf>,
    pub cnc_override: Option<String>,
    pub site_override: Option<String>,
    pub anchor_override: Option<chrono::DateTime<chrono::Utc>>,
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
