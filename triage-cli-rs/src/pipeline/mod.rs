//! End-to-end pipeline that turns a fetched ticket into a five-markdown
//! ticket folder (spec § 4). The `Reporter` trait decouples progress output
//! from orchestration: `StderrReporter` (default), `SilentReporter`
//! (tests/watcher), `ChannelReporter` (TUI).

mod base_evidence;
mod ctx;
mod followup;
mod investigate;
mod options;
mod owner;
mod phases;
mod reporter;
mod revise;
mod site;

pub use followup::{followup_turn, followup_turn_with_cancel, SESSION_LOST_ACTION};
pub use investigate::investigate_one_structured;
pub use options::{InvestigateOptions, StructuredInvestigation};
pub use reporter::{
    spinner, ChannelReporter, MetricValue, MetricsReporter, Reporter, SilentReporter,
    SpinnerReporter, StderrReporter, TuiEvent,
};
pub use revise::revise;
pub use site::resolve_site;

use thiserror::Error;

use crate::datadog::DatadogError;
use crate::extract::ExtractError;
use crate::llm::LlmError;
use crate::memory;
use crate::ticket_folder::TicketFolderError;
use crate::zendesk::ZendeskError;

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
    LockContention(std::path::PathBuf),
    #[error("base snapshot missing or unreadable: {0}")]
    BaseSnapshotMissing(String),
    #[error(transparent)]
    Chat(#[from] crate::chat::ChatError),
    #[error(transparent)]
    Provider(#[from] crate::providers::ProviderError),
}
