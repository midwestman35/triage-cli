//! Bundled fork rubric and rubric loader.
//!
//! The rubric is the playbook side of the engine/playbook split (spec
//! `docs/spec/v1-reframe.md`, Anchor A). The engine loads it as data and
//! never branches on customer-specific logic in code.
//!
//! The default rubric is embedded at build time via `include_str!`. Setting
//! `TRIAGE_RUBRIC_PATH` overrides it from disk — useful while iterating on
//! the rubric file without rebuilding the binary.

use std::path::PathBuf;

use once_cell::sync::Lazy;
use regex::Regex;
use thiserror::Error;

const EMBEDDED_RUBRIC: &str = include_str!("../playbook/fork-rubric.md");

pub const RUBRIC_PATH_ENV: &str = "TRIAGE_RUBRIC_PATH";

#[derive(Debug, Error)]
pub enum PlaybookError {
    #[error("rubric override path {0} could not be read: {1}")]
    OverrideReadFailed(PathBuf, std::io::Error),
    #[error("rubric is missing the `rubric_version` frontmatter field")]
    VersionMissing,
}

#[derive(Debug, Clone)]
pub struct Rubric {
    text: String,
    version: String,
}

impl Rubric {
    /// Load the rubric: from `TRIAGE_RUBRIC_PATH` if set, else the embedded
    /// copy compiled into the binary.
    pub fn load() -> Result<Self, PlaybookError> {
        let text = match std::env::var(RUBRIC_PATH_ENV).ok() {
            Some(path) => {
                let path = PathBuf::from(path);
                std::fs::read_to_string(&path)
                    .map_err(|e| PlaybookError::OverrideReadFailed(path, e))?
            }
            None => EMBEDDED_RUBRIC.to_string(),
        };
        let version = parse_version(&text).ok_or(PlaybookError::VersionMissing)?;
        Ok(Self { text, version })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// Soft-warn validator helper: returns true if `quoted` is a verbatim
    /// substring of the rubric text. Strictness is deliberately weak in v1
    /// (spec section 10, decision 1) — the caller logs a warning on miss
    /// rather than rejecting the LLM's response.
    pub fn contains_row(&self, quoted: &str) -> bool {
        if quoted.trim().is_empty() {
            return false;
        }
        self.text.contains(quoted)
    }
}

static VERSION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^rubric_version:\s*"?([^"\n]+?)"?\s*$"#).unwrap());

fn parse_version(text: &str) -> Option<String> {
    VERSION_RE
        .captures(text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_rubric_loads_with_version() {
        let r = Rubric::load().expect("embedded rubric should load");
        assert!(!r.text().is_empty(), "rubric text is empty");
        assert!(!r.version().is_empty(), "rubric version is empty");
    }

    #[test]
    fn version_parser_handles_quoted_form() {
        assert_eq!(
            parse_version("rubric_version: \"2026-05-13\""),
            Some("2026-05-13".into())
        );
    }

    #[test]
    fn version_parser_handles_unquoted_form() {
        assert_eq!(
            parse_version("rubric_version: 2026-05-13"),
            Some("2026-05-13".into())
        );
    }

    #[test]
    fn version_parser_handles_trailing_whitespace() {
        assert_eq!(
            parse_version("rubric_version: 2026-05-13   "),
            Some("2026-05-13".into())
        );
    }

    #[test]
    fn missing_version_returns_none() {
        assert_eq!(parse_version("# Some doc\nno version here"), None);
    }

    #[test]
    fn contains_row_matches_substring_from_embedded() {
        let r = Rubric::load().expect("load");
        assert!(r.contains_row("customer LAN, switch, or SDWAN. Link to site master ticket"));
    }

    #[test]
    fn contains_row_rejects_unknown_text() {
        let r = Rubric::load().expect("load");
        assert!(!r.contains_row("this string is definitely not in the rubric"));
    }

    #[test]
    fn contains_row_rejects_empty_and_whitespace() {
        let r = Rubric::load().expect("load");
        assert!(!r.contains_row(""));
        assert!(!r.contains_row("   \n\t  "));
    }
}
