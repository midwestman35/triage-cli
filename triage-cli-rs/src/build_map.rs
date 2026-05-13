//! Port of `scripts/build_cnc_map.py`.
//!
//! Reads `apex-cnc-inventory.md`, parses two markdown tables under fixed H2
//! headings, dedupes/validates rows, and writes:
//!   - `data/cnc-map.json`            (sorted entries)
//!   - `data/cnc-map-gaps.md`         (rows that couldn't be converted)
//!
//! Conversion rules (preserve byte-for-byte):
//!   - UUID column must match `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`
//!     (case-insensitive). Non-matching rows land in the "blank" gap section.
//!   - Per-site table wins where CNCs collide; master-table dupes are silently
//!     dropped.
//!   - Master-table labels must already look like a `site_name` (no spaces and
//!     match `^[a-z]{2}-[a-z0-9-]+$` after lowercasing). Otherwise they land in
//!     the "unparseable_label" gap section.
//!   - Friendly names from the per-site table get a trailing parenthetical
//!     stripped (e.g. "Fairfax Pine Ridge (page lists ...)" → "Fairfax Pine Ridge").

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use chrono::{SecondsFormat, Utc};
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

const INVENTORY: &str = "apex-cnc-inventory.md";
const MAP_OUT: &str = "data/cnc-map.json";
const GAPS_OUT: &str = "data/cnc-map-gaps.md";

const PER_SITE_HEADING: &str = "Confirmed via per-site Overview pages";
const MASTER_HEADING: &str = "From master \"APEX Sites Description\" page (display label only)";

static UUID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").unwrap()
});
static SITE_NAME_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[a-z]{2}-[a-z0-9-]+$").unwrap());
static TRAILING_PARENS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s*\([^)]*\)\s*$").unwrap());

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CncEntry {
    pub friendly_name: String,
    pub site_name: String,
    pub cnc: String,
}

#[derive(Debug, Default)]
pub struct Gaps {
    pub blank: Vec<BlankGap>,
    pub unparseable_label: Vec<UnparseableGap>,
}

#[derive(Debug)]
pub struct BlankGap {
    pub site_name: String,
    pub friendly_name: String,
    pub notes: String,
}

#[derive(Debug)]
pub struct UnparseableGap {
    pub display_label: String,
    pub cnc: String,
    pub region: String,
}

fn split_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}

fn is_separator(line: &str) -> bool {
    let inner = line.trim().trim_matches('|');
    if inner.is_empty() {
        return false;
    }
    inner.split('|').all(|cell| {
        let trimmed = cell.trim();
        !trimmed.is_empty() && trimmed.chars().all(|c| c == '-' || c == ':')
    })
}

/// Return data rows (cells) under the H2 whose title matches `heading`.
pub fn parse_table(text: &str, heading: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut in_section = false;
    let mut seen_separator = false;
    for line in text.lines() {
        let stripped = line.trim();
        if let Some(rest) = stripped.strip_prefix("## ") {
            if in_section {
                break;
            }
            in_section = rest.trim() == heading;
            continue;
        }
        if !in_section || !stripped.starts_with('|') {
            continue;
        }
        if is_separator(stripped) {
            seen_separator = true;
            continue;
        }
        if seen_separator {
            rows.push(split_row(stripped));
        }
    }
    rows
}

fn clean_friendly(name: &str) -> String {
    TRAILING_PARENS_RE.replace(name, "").trim().to_string()
}

fn is_uuid(value: &str) -> bool {
    UUID_RE.is_match(value.trim().to_ascii_lowercase().as_str())
}

fn normalize_label_to_site_name(label: &str) -> Option<String> {
    let trimmed = label.trim();
    if trimmed.contains(' ') {
        return None;
    }
    let candidate = trimmed.to_ascii_lowercase();
    if SITE_NAME_RE.is_match(&candidate) {
        Some(candidate)
    } else {
        None
    }
}

pub fn build_entries(
    per_site_rows: &[Vec<String>],
    master_rows: &[Vec<String>],
) -> (Vec<CncEntry>, Gaps) {
    // Insertion-ordered map so per-site processing produces stable output
    // before we sort by site_name at the end.
    let mut entries: indexmap::IndexMap<String, CncEntry> = indexmap::IndexMap::new();
    let mut gaps = Gaps::default();

    for row in per_site_rows {
        if row.len() < 3 {
            continue;
        }
        let site_name = &row[0];
        let friendly = &row[1];
        let cnc = &row[2];
        if !is_uuid(cnc) {
            gaps.blank.push(BlankGap {
                site_name: site_name.clone(),
                friendly_name: friendly.clone(),
                notes: cnc.clone(),
            });
            continue;
        }
        let cnc_key = cnc.to_ascii_lowercase();
        entries.insert(
            cnc_key.clone(),
            CncEntry {
                friendly_name: clean_friendly(friendly),
                site_name: site_name.clone(),
                cnc: cnc_key,
            },
        );
    }

    for row in master_rows {
        if row.len() < 3 {
            continue;
        }
        let label = &row[0];
        let cnc = &row[1];
        let region = &row[2];
        if !is_uuid(cnc) {
            gaps.blank.push(BlankGap {
                site_name: String::new(),
                friendly_name: label.clone(),
                notes: cnc.clone(),
            });
            continue;
        }
        let cnc_key = cnc.to_ascii_lowercase();
        if entries.contains_key(&cnc_key) {
            continue; // per-site wins
        }
        let Some(site_name) = normalize_label_to_site_name(label) else {
            gaps.unparseable_label.push(UnparseableGap {
                display_label: label.clone(),
                cnc: cnc_key,
                region: region.clone(),
            });
            continue;
        };
        entries.insert(
            cnc_key.clone(),
            CncEntry {
                friendly_name: label.clone(),
                site_name,
                cnc: cnc_key,
            },
        );
    }

    let mut entries: Vec<CncEntry> = entries.into_values().collect();
    entries.sort_by(|a, b| a.site_name.cmp(&b.site_name));
    (entries, gaps)
}

fn render_section(heading: &str, header: &str, rows: &[String]) -> Vec<String> {
    let mut out = vec![format!("## {heading}"), String::new()];
    if rows.is_empty() {
        out.push("_None._".into());
    } else {
        out.push(header.to_string());
        let cols = header.matches('|').count().saturating_sub(1);
        out.push(format!("|{}", "---|".repeat(cols)));
        out.extend(rows.iter().cloned());
    }
    out.push(String::new());
    out
}

pub fn render_gaps_markdown(gaps: &Gaps) -> String {
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut lines: Vec<String> = vec![
        "# CNC map gaps".into(),
        String::new(),
        "Rows from `apex-cnc-inventory.md` that were skipped during conversion. Fill in".into(),
        "the missing data and re-run `triage-cli build-map` (or".into(),
        "`python scripts/build_cnc_map.py`) to regenerate `data/cnc-map.json`.".into(),
        String::new(),
        format!("Generated: {now}"),
        "Source: `apex-cnc-inventory.md`".into(),
        String::new(),
    ];

    let blank_rows: Vec<String> = gaps
        .blank
        .iter()
        .map(|r| format!("| {} | {} | {} |", r.site_name, r.friendly_name, r.notes))
        .collect();
    lines.extend(render_section(
        "Missing or unparseable CNC UUID in source",
        "| Site Name | Friendly Name | Notes |",
        &blank_rows,
    ));

    let label_rows: Vec<String> = gaps
        .unparseable_label
        .iter()
        .map(|r| format!("| {} | {} | {} |", r.display_label, r.cnc, r.region))
        .collect();
    lines.extend(render_section(
        "Master-table label is not a parseable site_name",
        "| Display Label | CNC UUID | Region |",
        &label_rows,
    ));

    lines.join("\n")
}

/// Run the build_map flow. Returns an `ExitCode` so callers can propagate
/// success/failure to the process.
pub fn run() -> ExitCode {
    let inventory_path = Path::new(INVENTORY);
    let Ok(raw_bytes) = fs::read(inventory_path) else {
        eprintln!(
            "Error: could not read {} from the current working directory.",
            INVENTORY
        );
        return ExitCode::FAILURE;
    };
    // Python read with encoding="utf-8-sig" to drop a BOM if present.
    let inventory_text = strip_bom(&raw_bytes);

    let per_site_rows = parse_table(&inventory_text, PER_SITE_HEADING);
    let master_rows = parse_table(&inventory_text, MASTER_HEADING);
    if per_site_rows.is_empty() && master_rows.is_empty() {
        eprintln!(
            "No table rows found under either '{PER_SITE_HEADING}' or '{MASTER_HEADING}' in {INVENTORY}. \
             Headings may have changed; refusing to overwrite {MAP_OUT}."
        );
        return ExitCode::FAILURE;
    }

    let (entries, gaps) = build_entries(&per_site_rows, &master_rows);
    let map_path = Path::new(MAP_OUT);
    let gaps_path = Path::new(GAPS_OUT);
    if let Some(parent) = map_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("Error: could not create {}: {e}", parent.display());
            return ExitCode::FAILURE;
        }
    }
    let json = match serde_json::to_string_pretty(&entries) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: could not serialize CNC entries: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = fs::write(map_path, format!("{json}\n")) {
        eprintln!("Error: could not write {}: {e}", map_path.display());
        return ExitCode::FAILURE;
    }
    let gaps_md = render_gaps_markdown(&gaps);
    if let Err(e) = fs::write(gaps_path, gaps_md) {
        eprintln!("Error: could not write {}: {e}", gaps_path.display());
        return ExitCode::FAILURE;
    }
    let gap_total = gaps.blank.len() + gaps.unparseable_label.len();
    println!(
        "Wrote {} entries to {}",
        entries.len(),
        map_path.display()
    );
    println!(
        "Logged {gap_total} gaps to {}",
        gaps_path.display()
    );
    ExitCode::SUCCESS
}

fn strip_bom(bytes: &[u8]) -> String {
    let utf8 = String::from_utf8_lossy(bytes).into_owned();
    utf8.strip_prefix('\u{feff}')
        .map(str::to_string)
        .unwrap_or(utf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_simple_table() {
        let md = "\
# title

## Confirmed via per-site Overview pages

| Site Name | Friendly Name | CNC UUID |
|---|---|---|
| us-nv-acme | Acme PD | de9ee414-da5a-471d-bac2-10643190da0b |
| us-co-aurora-apex | Aurora 911, CO | 921d7c53-e815-4566-9692-6cbce589e1d3 |

## Other heading
";
        let rows = parse_table(md, "Confirmed via per-site Overview pages");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], "us-nv-acme");
        assert_eq!(rows[1][1], "Aurora 911, CO");
    }

    #[test]
    fn per_site_wins_over_master() {
        let per_site = vec![vec![
            "us-co-aurora-apex".into(),
            "Aurora 911".into(),
            "921d7c53-e815-4566-9692-6cbce589e1d3".into(),
        ]];
        let master = vec![vec![
            "us-co-aurora-apex".into(),
            "921d7c53-e815-4566-9692-6cbce589e1d3".into(),
            "Colorado".into(),
        ]];
        let (entries, gaps) = build_entries(&per_site, &master);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].friendly_name, "Aurora 911");
        assert!(gaps.blank.is_empty());
        assert!(gaps.unparseable_label.is_empty());
    }

    #[test]
    fn master_label_with_space_falls_into_unparseable() {
        let master = vec![vec![
            "MX-Sales CCS".into(),
            "921d7c53-e815-4566-9692-6cbce589e1d3".into(),
            "Mexico".into(),
        ]];
        let (entries, gaps) = build_entries(&[], &master);
        assert!(entries.is_empty());
        assert_eq!(gaps.unparseable_label.len(), 1);
    }

    #[test]
    fn missing_uuid_lands_in_blank() {
        let per_site = vec![vec![
            "us-wa-spokane-apex".into(),
            "City of Spokane".into(),
            "(CNC field blank in source page)".into(),
        ]];
        let (entries, gaps) = build_entries(&per_site, &[]);
        assert!(entries.is_empty());
        assert_eq!(gaps.blank.len(), 1);
    }

    #[test]
    fn trailing_parenthetical_is_stripped() {
        assert_eq!(
            clean_friendly("Fairfax Pine Ridge (page lists \"Fairfax George Mason University\")"),
            "Fairfax Pine Ridge"
        );
    }
}
