//! Fixture / demo mode: run the pipeline end-to-end without real credentials.
//!
//! A fixture directory contains pre-canned inputs used by `triage-cli demo`
//! and `triage-cli triage --fixture <path>`. The `expected/` subdirectory
//! is reserved for golden-output tests (roadmap item #3).

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use thiserror::Error;

use crate::datadog::{DatadogSource, LogsFuture};
use crate::models::{LogLine, MemoryEntry, Ticket};

#[derive(Debug, Error)]
pub enum FixtureError {
    #[error("fixture directory not found: {0}")]
    DirNotFound(PathBuf),
    #[error("fixture file not found: {0}")]
    FileNotFound(PathBuf),
    #[error("fixture parse error in {path}: {source}")]
    ParseError {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("IO error reading fixture: {0}")]
    Io(#[from] std::io::Error),
}

/// Built-in fixture names shipped with the crate.
pub const NAMED_FIXTURES: &[&str] = &[
    "audio-drop",
    "no-site-map",
    "missing-evidence",
    "vendor-fork",
];

/// Root directory that contains the named fixture subdirectories.
///
/// Resolution order:
/// 1. `TRIAGE_FIXTURES_DIR` env var
/// 2. A `fixtures/` directory adjacent to the running binary
/// 3. `./fixtures/` (relative to cwd; typical when running from `triage-cli-rs/`)
/// 4. `./triage-cli-rs/fixtures/` (when running from the repo root)
pub fn fixtures_root() -> PathBuf {
    if let Ok(p) = std::env::var("TRIAGE_FIXTURES_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.parent().unwrap_or(Path::new(".")).join("fixtures");
        if sibling.is_dir() {
            return sibling;
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let in_crate = cwd.join("fixtures");
    if in_crate.is_dir() {
        return in_crate;
    }
    cwd.join("triage-cli-rs").join("fixtures")
}

/// Resolve a friendly fixture name (e.g. `"audio-drop"`) to an absolute path.
pub fn resolve_named(name: &str) -> PathBuf {
    fixtures_root().join(name)
}

/// Loads a fixture directory's canned inputs.
pub struct FixtureLoader {
    pub dir: PathBuf,
}

impl FixtureLoader {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self, FixtureError> {
        let dir = dir.into();
        if !dir.is_dir() {
            return Err(FixtureError::DirNotFound(dir));
        }
        Ok(Self { dir })
    }

    /// Load the `Ticket` from `ticket.json`.
    pub fn load_ticket(&self) -> Result<Ticket, FixtureError> {
        self.load_required_json("ticket.json")
    }

    /// Load Datadog log lines from `datadog-logs.json`. Returns empty vec if the file is absent.
    pub fn load_datadog_logs(&self) -> Result<Vec<LogLine>, FixtureError> {
        self.load_optional_json("datadog-logs.json")
    }

    /// Load prior investigation hits from `memory-hits.json`. Returns empty vec if absent.
    pub fn load_memory_hits(&self) -> Result<Vec<MemoryEntry>, FixtureError> {
        self.load_optional_json("memory-hits.json")
    }

    fn load_required_json<T: serde::de::DeserializeOwned>(
        &self,
        filename: &str,
    ) -> Result<T, FixtureError> {
        let path = self.dir.join(filename);
        if !path.exists() {
            return Err(FixtureError::FileNotFound(path));
        }
        self.parse_json_file(&path)
    }

    fn load_optional_json<T: serde::de::DeserializeOwned + Default>(
        &self,
        filename: &str,
    ) -> Result<T, FixtureError> {
        let path = self.dir.join(filename);
        if !path.exists() {
            return Ok(T::default());
        }
        self.parse_json_file(&path)
    }

    fn parse_json_file<T: serde::de::DeserializeOwned>(
        &self,
        path: &Path,
    ) -> Result<T, FixtureError> {
        let text = fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(|e| FixtureError::ParseError {
            path: path.to_path_buf(),
            source: e,
        })
    }
}

/// A canned Datadog client that returns pre-loaded log lines regardless of the
/// query parameters. Used by fixture and demo mode.
pub struct FixtureDatadogClient {
    logs: Vec<LogLine>,
    truncated: bool,
}

impl FixtureDatadogClient {
    pub fn new(logs: Vec<LogLine>) -> Self {
        let truncated = logs.len() >= 200;
        Self { logs, truncated }
    }
}

impl DatadogSource for FixtureDatadogClient {
    fn get_logs<'a>(
        &'a self,
        _site_name: &'a str,
        _levels: &'a [String],
        _start: DateTime<Utc>,
        _end: DateTime<Utc>,
    ) -> LogsFuture<'a> {
        let result = Ok((self.logs.clone(), self.truncated));
        Box::pin(async move { result })
    }

    fn get_logs_for_query<'a>(
        &'a self,
        _query: &'a crate::datadog::DatadogQuery,
        _start: DateTime<Utc>,
        _end: DateTime<Utc>,
    ) -> LogsFuture<'a> {
        let result = Ok((self.logs.clone(), self.truncated));
        Box::pin(async move { result })
    }
}
