use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

/// Parsed `STATE.md` frontmatter. This is a superset of
/// `ticket_folder::ExistingState`, with the `related` block parsed so the inbox
/// can render Zendesk/Jira IDs in the synth summary.
#[derive(Debug, Clone, Default)]
pub struct InboxStateSummary {
    pub ticket_id: Option<u64>,
    pub fork: Option<String>,
    pub confidence: Option<String>,
    pub status: Option<String>,
    pub owner: Option<String>,
    pub quoted_rubric_row: Option<String>,
    pub rubric_version: Option<String>,
    pub related_zendesk: Vec<u64>,
    pub related_jira: Vec<String>,
    pub master: Option<u64>,
    pub cluster: Option<String>,
    pub validator_warnings: Vec<String>,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Scan a `tickets_root` directory and return one entry per subdirectory that
/// contains a readable `STATE.md`. Entries with non-numeric folder names are
/// skipped because the spec requires `Tickets/<zendesk_id>/`.
pub fn scan_tickets_root(root: &Path) -> Vec<(u64, PathBuf, InboxStateSummary)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(id) = name.parse::<u64>() else {
            continue;
        };
        let state_path = path.join("STATE.md");
        if !state_path.is_file() {
            continue;
        }
        let summary = parse_state_md(&state_path).unwrap_or_default();
        out.push((id, path, summary));
    }
    out.sort_by_key(|(id, _, _)| *id);
    out
}

/// Parse a `STATE.md` file into the inbox summary view. Best-effort: missing
/// or malformed fields are returned as `None` / empty vectors; only an
/// unreadable file yields `None`.
pub fn parse_state_md(state_path: &Path) -> Option<InboxStateSummary> {
    let text = std::fs::read_to_string(state_path).ok()?;
    Some(parse_state_md_str(&text))
}

/// String-input variant exposed so tests do not need a tempdir.
pub fn parse_state_md_str(text: &str) -> InboxStateSummary {
    let mut s = InboxStateSummary::default();
    let mut in_related = false;
    let mut in_validator = false;
    for line in text.lines() {
        if line.trim() == "---" {
            continue;
        }

        let is_indented = line.starts_with([' ', '\t']);

        if !is_indented {
            in_related = false;
            in_validator = false;
        }

        if is_indented && in_validator {
            if let Some(rest) = line.trim_start().strip_prefix("- ") {
                if let Some(item) = strip_yaml_scalar(rest.trim()) {
                    s.validator_warnings.push(item);
                }
            }
            continue;
        }

        let (raw_key, raw_value) = match line.split_once(':') {
            Some(pair) => pair,
            None => continue,
        };
        let key = raw_key.trim();
        let value = raw_value.trim();

        if !is_indented {
            match key {
                "ticket_id" => s.ticket_id = value.parse().ok(),
                "fork" => s.fork = strip_yaml_scalar(value),
                "confidence" => s.confidence = strip_yaml_scalar(value),
                "status" => s.status = strip_yaml_scalar(value),
                "owner" => s.owner = strip_yaml_scalar(value),
                "quoted_rubric_row" => s.quoted_rubric_row = strip_yaml_scalar(value),
                "rubric_version" => s.rubric_version = strip_yaml_scalar(value),
                "cluster" => s.cluster = strip_yaml_scalar(value),
                "updated_at" | "created_at" => {
                    let candidate = strip_yaml_scalar(value).unwrap_or_default();
                    if let Ok(parsed) = DateTime::parse_from_rfc3339(&candidate) {
                        s.updated_at = Some(parsed.with_timezone(&Utc));
                    }
                }
                "related" => in_related = true,
                "validator_warnings" => {
                    if value.starts_with('[') {
                        s.validator_warnings = parse_inline_str_list(value);
                    } else {
                        in_validator = true;
                    }
                }
                _ => {}
            }
            continue;
        }

        if in_related {
            match key.trim() {
                "zendesk" => s.related_zendesk = parse_inline_u64_list(value),
                "jira" => s.related_jira = parse_inline_str_list(value),
                "master" => {
                    let v = strip_yaml_scalar(value);
                    s.master = v.and_then(|x| x.parse().ok());
                }
                _ => {}
            }
        }
    }
    s
}

fn strip_yaml_scalar(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s == "null" || s == "~" {
        return None;
    }
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let inner = &s[1..s.len() - 1];
        return Some(inner.replace(r#"\""#, "\"").replace(r"\\", "\\"));
    }
    Some(s.to_string())
}

fn parse_inline_u64_list(value: &str) -> Vec<u64> {
    let v = value.trim();
    if !(v.starts_with('[') && v.ends_with(']')) {
        return Vec::new();
    }
    v[1..v.len() - 1]
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u64>().ok())
        .collect()
}

fn parse_inline_str_list(value: &str) -> Vec<String> {
    let v = value.trim();
    if !(v.starts_with('[') && v.ends_with(']')) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_string = false;
    let mut escape = false;
    for ch in v[1..v.len() - 1].chars() {
        if escape {
            buf.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            if !in_string {
                out.push(std::mem::take(&mut buf));
            }
            continue;
        }
        if in_string {
            buf.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_state_md() -> &'static str {
        r#"---
ticket_id: 44671
fork: B
confidence: medium
quoted_rubric_row: "customer LAN, switch, or SDWAN. Link to site master ticket"
rubric_version: "2026-04-30"
owner: "alice@example.com"
created_at: 2026-05-13T07:32:11Z
updated_at: 2026-05-13T07:32:11Z
status: open
related:
  zendesk: [43874, 42708]
  jira: ["REP-1234", "REP-5678"]
  master: null
cluster: "jeffcom-network-error"
validator_warnings: ["quoted_rubric_row paraphrased"]
---
"#
    }

    #[test]
    fn parse_state_md_extracts_top_level_scalars() {
        let s = parse_state_md_str(sample_state_md());
        assert_eq!(s.ticket_id, Some(44671));
        assert_eq!(s.fork.as_deref(), Some("B"));
        assert_eq!(s.confidence.as_deref(), Some("medium"));
        assert_eq!(s.status.as_deref(), Some("open"));
        assert_eq!(s.owner.as_deref(), Some("alice@example.com"));
        assert_eq!(
            s.quoted_rubric_row.as_deref(),
            Some("customer LAN, switch, or SDWAN. Link to site master ticket")
        );
        assert_eq!(s.rubric_version.as_deref(), Some("2026-04-30"));
        assert_eq!(s.cluster.as_deref(), Some("jeffcom-network-error"));
    }

    #[test]
    fn parse_state_md_parses_related_lists() {
        let s = parse_state_md_str(sample_state_md());
        assert_eq!(s.related_zendesk, vec![43874, 42708]);
        assert_eq!(s.related_jira, vec!["REP-1234", "REP-5678"]);
        assert!(s.master.is_none());
    }

    #[test]
    fn parse_state_md_parses_validator_warnings_inline() {
        let s = parse_state_md_str(sample_state_md());
        assert_eq!(
            s.validator_warnings,
            vec!["quoted_rubric_row paraphrased".to_string()]
        );
    }

    #[test]
    fn parse_state_md_parses_validator_warnings_block_form() {
        let text = r#"---
ticket_id: 1
fork: A
validator_warnings:
  - "first warning"
  - "second warning"
---
"#;
        let s = parse_state_md_str(text);
        assert_eq!(
            s.validator_warnings,
            vec!["first warning".to_string(), "second warning".to_string()]
        );
    }

    #[test]
    fn parse_state_md_handles_missing_optional_fields() {
        let text = r#"---
ticket_id: 12345
fork: D
confidence: low
status: open
---
"#;
        let s = parse_state_md_str(text);
        assert_eq!(s.ticket_id, Some(12345));
        assert_eq!(s.fork.as_deref(), Some("D"));
        assert!(s.owner.is_none());
        assert!(s.quoted_rubric_row.is_none());
        assert!(s.related_zendesk.is_empty());
        assert!(s.master.is_none());
    }

    #[test]
    fn parse_state_md_treats_null_as_none() {
        let text = "---\nticket_id: 1\nfork: A\ncluster: null\n---\n";
        let s = parse_state_md_str(text);
        assert!(s.cluster.is_none());
    }

    #[test]
    fn scan_tickets_root_returns_only_dirs_with_state_md() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let ok = root.join("44671");
        std::fs::create_dir_all(&ok).unwrap();
        std::fs::write(ok.join("STATE.md"), sample_state_md()).unwrap();

        let no_state = root.join("99999");
        std::fs::create_dir_all(&no_state).unwrap();
        std::fs::write(no_state.join("INTAKE.md"), "x").unwrap();

        let weird = root.join("not-a-ticket");
        std::fs::create_dir_all(&weird).unwrap();
        std::fs::write(weird.join("STATE.md"), sample_state_md()).unwrap();

        std::fs::write(root.join("stray.md"), "x").unwrap();

        let entries = scan_tickets_root(root);
        let ids: Vec<u64> = entries.iter().map(|(id, _, _)| *id).collect();
        assert_eq!(ids, vec![44671], "got entries: {entries:?}");
    }

    #[test]
    fn scan_tickets_root_parses_state_into_summary() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let folder = root.join("12345");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("STATE.md"), sample_state_md()).unwrap();

        let entries = scan_tickets_root(root);
        assert_eq!(entries.len(), 1);
        let (id, path, summary) = &entries[0];
        assert_eq!(*id, 12345);
        assert_eq!(path, &folder);
        assert_eq!(summary.fork.as_deref(), Some("B"));
        assert_eq!(
            summary.quoted_rubric_row.as_deref(),
            Some("customer LAN, switch, or SDWAN. Link to site master ticket")
        );
    }
}
