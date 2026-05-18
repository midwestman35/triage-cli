//! `triage-cli setup` and `triage-cli doctor` — onboarding + health check.
//!
//! `setup` is interactive: it prompts for required env vars, writes a `.env`
//! file in the working directory, and optionally rebuilds the site map. `doctor`
//! is a non-destructive read-only probe and matches the Python implementation.

use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use dialoguer::{Input, Password, Select};
use owo_colors::OwoColorize;

const ENV_PATH: &str = ".env";

pub async fn doctor() -> ExitCode {
    let mut ok = true;
    let home = crate::paths::triage_home();
    eprintln!("triage_home: {}", home.display());
    eprintln!("  .env:                  {}", home.join(".env").display());
    eprintln!(
        "  MEMORY.md:             {}",
        home.join("MEMORY.md").display()
    );
    eprintln!(
        "  apex-cnc-inventory.md: {}",
        home.join("apex-cnc-inventory.md").display()
    );
    eprintln!(
        "  data/cnc-map.json:     {}",
        home.join("data/cnc-map.json").display()
    );
    eprintln!(
        "  data/memory.db:        {}",
        home.join("data/memory.db").display()
    );
    eprintln!(
        "  Tickets/:              {}",
        crate::ticket_folder::tickets_root().display()
    );
    eprintln!();

    eprintln!("Zendesk:");
    for var in ["ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN"] {
        if env::var(var).map(|v| !v.is_empty()).unwrap_or(false) {
            eprintln!("  {} {}", "✓".green(), var);
        } else {
            eprintln!("  {} {} not set", "✗".red(), var);
            ok = false;
        }
    }

    let provider = env::var("LLM_PROVIDER").unwrap_or_else(|_| "unleash".into());
    eprintln!("LLM provider: {provider}");
    match provider.to_ascii_lowercase().as_str() {
        "unleash" => {
            check_env("UNLEASH_API_KEY", &provider, &mut ok);
            check_env("UNLEASH_ASSISTANT_ID", &provider, &mut ok);
        }
        "codex" => {
            // Subprocess provider — verify the binary is on PATH.
            if which::which("codex").is_ok() {
                eprintln!("  {} `codex` on PATH (subprocess provider)", "✓".green());
            } else {
                eprintln!(
                    "  {} `codex` not on PATH (required for LLM_PROVIDER=codex)",
                    "✗".red()
                );
                ok = false;
            }
        }
        other => {
            eprintln!(
                "  {} LLM_PROVIDER={other:?} is not a recognized provider (valid: unleash, codex)",
                "✗".red()
            );
            ok = false;
        }
    }

    let scratch_dir = crate::paths::triage_home().join("scratch");
    match probe_writable(&scratch_dir) {
        Ok(_) => eprintln!("  {} <triage-home>/scratch/ writable", "✓".green()),
        Err(e) => {
            eprintln!("  {} <triage-home>/scratch/ not writable: {e}", "✗".red());
            ok = false;
        }
    }

    let dd_ok = env::var("DD_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        && env::var("DD_APP_KEY")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
    if dd_ok {
        eprintln!(
            "  {} Datadog configured (DD_API_KEY, DD_APP_KEY)",
            "✓".green()
        );
    } else {
        eprintln!(
            "  {} Datadog not configured — --no-logs will be forced",
            "⚠".yellow()
        );
    }

    let inv = home.join("apex-cnc-inventory.md");
    let map = home.join("data/cnc-map.json");
    if let (Ok(inv_md), Ok(map_md)) = (std::fs::metadata(&inv), std::fs::metadata(&map)) {
        if let (Ok(inv_mt), Ok(map_mt)) = (inv_md.modified(), map_md.modified()) {
            if inv_mt > map_mt {
                eprintln!(
                    "{}: cnc-map is stale; run triage-cli build-map to refresh.",
                    "warning".yellow().bold()
                );
            }
        }
    }

    if let Some(newer) = check_for_update().await {
        eprintln!(
            "{}: update available: {} (you have {}). re-run install.ps1 (or install.sh) to upgrade.",
            "note".yellow().bold(),
            newer,
            env!("CARGO_PKG_VERSION"),
        );
    }

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn check_env(var: &'static str, provider: &str, ok: &mut bool) {
    if env::var(var).map(|v| !v.is_empty()).unwrap_or(false) {
        eprintln!("  {} {}", "✓".green(), var);
    } else {
        eprintln!(
            "  {} {} not set (required for LLM_PROVIDER={provider})",
            "✗".red(),
            var
        );
        *ok = false;
    }
}

fn probe_writable(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let probe = dir.join(".doctor-probe");
    fs::write(&probe, b"")?;
    fs::remove_file(&probe)
}

/// Interactive first-run setup. Idempotent: existing values become defaults.
pub fn setup() -> ExitCode {
    eprintln!("{} triage-cli setup", "→".cyan());
    eprintln!(
        "Will prompt for credentials and write them to {}.",
        ENV_PATH
    );

    let env_path_buf = crate::paths::triage_home().join(ENV_PATH);
    let existing = read_env_file(env_path_buf.as_path());
    let zd_subdomain = prompt_text("Zendesk subdomain", existing.get("ZENDESK_SUBDOMAIN"));
    let zd_email = prompt_text("Zendesk agent email", existing.get("ZENDESK_EMAIL"));
    let zd_token = prompt_secret("Zendesk API token", existing.get("ZENDESK_API_TOKEN"));

    let providers = ["unleash", "codex"];
    let default_provider = existing
        .get("LLM_PROVIDER")
        .cloned()
        .unwrap_or_else(|| "unleash".into());
    let default_idx = providers
        .iter()
        .position(|p| p == &default_provider.as_str())
        .unwrap_or(0);
    let provider_choice = Select::new()
        .with_prompt("LLM provider")
        .items(&providers)
        .default(default_idx)
        .interact()
        .unwrap_or(0);
    let provider = providers[provider_choice].to_string();

    let mut next: Vec<(String, String)> = vec![
        ("ZENDESK_SUBDOMAIN".into(), zd_subdomain),
        ("ZENDESK_EMAIL".into(), zd_email),
        ("ZENDESK_API_TOKEN".into(), zd_token),
        ("LLM_PROVIDER".into(), provider.clone()),
    ];
    if provider.as_str() == "unleash" {
        let key = prompt_secret("UNLEASH_API_KEY", existing.get("UNLEASH_API_KEY"));
        let aid = prompt_text("UNLEASH_ASSISTANT_ID", existing.get("UNLEASH_ASSISTANT_ID"));
        next.push(("UNLEASH_API_KEY".into(), key));
        next.push(("UNLEASH_ASSISTANT_ID".into(), aid));
    }

    let dd_key = prompt_text_optional(
        "DD_API_KEY (optional, blank to skip)",
        existing.get("DD_API_KEY"),
    );
    let dd_app = prompt_text_optional("DD_APP_KEY (optional)", existing.get("DD_APP_KEY"));
    if !dd_key.is_empty() {
        next.push(("DD_API_KEY".into(), dd_key));
    }
    if !dd_app.is_empty() {
        next.push(("DD_APP_KEY".into(), dd_app));
    }

    if let Err(e) = write_env_file(env_path_buf.as_path(), &next) {
        eprintln!("{} could not write {ENV_PATH}: {e}", "✗".red());
        return ExitCode::FAILURE;
    }
    eprintln!("{} wrote {} ({} keys)", "✓".green(), ENV_PATH, next.len());

    eprintln!();
    eprintln!(
        "{} regenerating data/cnc-map.json from inventory...",
        "→".cyan()
    );
    if crate::build_map::run() == ExitCode::SUCCESS {
        eprintln!("{} cnc-map regenerated", "✓".green());
    } else {
        eprintln!("{} build-map failed", "⚠".yellow());
        eprintln!("  (you can re-run `triage-cli build-map` manually)");
    }

    eprintln!("Run `triage-cli doctor` next to verify.");
    ExitCode::SUCCESS
}

fn prompt_text(label: &str, default: Option<&String>) -> String {
    let prompt = Input::<String>::new().with_prompt(label).allow_empty(false);
    let prompt = if let Some(d) = default.cloned() {
        prompt.default(d)
    } else {
        prompt
    };
    prompt.interact_text().unwrap_or_default()
}

fn prompt_text_optional(label: &str, default: Option<&String>) -> String {
    let prompt = Input::<String>::new().with_prompt(label).allow_empty(true);
    let prompt = if let Some(d) = default.cloned() {
        prompt.default(d)
    } else {
        prompt
    };
    prompt.interact_text().unwrap_or_default()
}

fn prompt_secret(label: &str, _default: Option<&String>) -> String {
    Password::new()
        .with_prompt(label)
        .allow_empty_password(false)
        .interact()
        .unwrap_or_default()
}

fn read_env_file(path: &Path) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Ok(text) = fs::read_to_string(path) else {
        return out;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_string(), unquote(v.trim()).to_string());
        }
    }
    out
}

fn unquote(s: &str) -> &str {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn write_env_file(path: &Path, entries: &[(String, String)]) -> std::io::Result<()> {
    // F4: merge into the existing file rather than truncating. Preserves
    // operator-added keys (TRIAGE_HOME, CODEX_MODEL, etc.) and any
    // hand-written comments. Atomic via tempfile + rename so a crash
    // mid-write can't leave a half-written .env.
    let existing = fs::read_to_string(path).unwrap_or_default();
    let merged = merge_env_content(&existing, entries);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    {
        use std::io::Write as _;
        tmp.as_file_mut().write_all(merged.as_bytes())?;
        tmp.as_file_mut().sync_all()?;
    }
    tmp.persist(path).map_err(std::io::Error::from)?;
    Ok(())
}

/// Format a single `KEY=value` line, quoting when the value contains a
/// space or `#` (otherwise unquoted, matching the pre-F4 writer).
fn render_env_line(k: &str, v: &str) -> String {
    if v.contains(' ') || v.contains('#') {
        format!("{k}=\"{v}\"")
    } else {
        format!("{k}={v}")
    }
}

/// Merge `updates` into the textual contents of an existing `.env` file,
/// returning the new file contents. The merge rules (F4):
///
/// 1. If `existing` is empty (no prior file), emit the
///    `# Generated by triage-cli setup` header followed by the updates.
/// 2. Otherwise, walk `existing` line-by-line:
///    - Comments and blank lines are preserved verbatim.
///    - A `KEY=...` line whose `KEY` is in `updates` is rewritten with the
///      new value (in-place, preserving its position).
///    - All other key lines are preserved verbatim (this is the F4 fix —
///      operator-added keys survive `setup` re-runs).
/// 3. Any update whose key was not present in `existing` is appended at
///    the end. Updates with empty values are skipped (matching the
///    pre-F4 behavior for unset optional prompts).
pub(crate) fn merge_env_content(existing: &str, updates: &[(String, String)]) -> String {
    let updates_map: std::collections::HashMap<&str, &str> = updates
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let mut written_keys = std::collections::HashSet::new();
    let mut out_lines: Vec<String> = Vec::new();

    if existing.is_empty() {
        out_lines.push("# Generated by triage-cli setup".to_string());
    } else {
        for line in existing.lines() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                out_lines.push(line.to_string());
                continue;
            }
            if let Some((k, _v)) = trimmed.split_once('=') {
                let key = k.trim();
                if let Some(new_val) = updates_map.get(key) {
                    if !new_val.is_empty() {
                        out_lines.push(render_env_line(key, new_val));
                    }
                    written_keys.insert(key.to_string());
                    continue;
                }
            }
            out_lines.push(line.to_string());
        }
    }

    for (k, v) in updates {
        if written_keys.contains(k) {
            continue;
        }
        if v.is_empty() {
            continue;
        }
        out_lines.push(render_env_line(k, v));
    }

    let mut out = out_lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Best-effort check against the latest GitHub Release tag. Returns the new
/// version string if a strictly-newer release exists, else `None`. Any
/// failure (network, timeout, JSON parse, semver compare, GH rate limit)
/// resolves to `None` — this is opportunistic icing on doctor, not a
/// critical check.
async fn check_for_update() -> Option<String> {
    let current = env!("CARGO_PKG_VERSION");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .user_agent(format!("triage-cli/{}", current))
        .build()
        .ok()?;
    let resp = client
        .get("https://api.github.com/repos/midwestman35/triage-cli/releases/latest")
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().await.ok()?;
    let tag = json.get("tag_name")?.as_str()?.trim_start_matches('v');
    if is_strictly_newer(tag, current) {
        Some(tag.to_string())
    } else {
        None
    }
}

/// Naive semver compare: split on `.`, compare numeric components
/// left-to-right. Returns true if `a` is strictly greater than `b`.
/// Pre-release suffixes (e.g., `-rc1`) are stripped before comparison —
/// we only nudge users between stable releases, not from `0.2.0-rc1` to
/// `0.2.0`.
fn is_strictly_newer(a: &str, b: &str) -> bool {
    fn parts(s: &str) -> Vec<u32> {
        s.split('-')
            .next()
            .unwrap_or("")
            .split('.')
            .filter_map(|p| p.parse().ok())
            .collect()
    }
    let ap = parts(a);
    let bp = parts(b);
    let n = ap.len().max(bp.len());
    for i in 0..n {
        let av = ap.get(i).copied().unwrap_or(0);
        let bv = bp.get(i).copied().unwrap_or(0);
        if av > bv {
            return true;
        }
        if av < bv {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod version_tests {
    use super::is_strictly_newer;

    #[test]
    fn newer_patch() {
        assert!(is_strictly_newer("0.2.1", "0.2.0"));
    }
    #[test]
    fn newer_minor() {
        assert!(is_strictly_newer("0.3.0", "0.2.5"));
    }
    #[test]
    fn same_version() {
        assert!(!is_strictly_newer("0.2.0", "0.2.0"));
    }
    #[test]
    fn older() {
        assert!(!is_strictly_newer("0.1.9", "0.2.0"));
    }
    #[test]
    fn pre_release_a() {
        assert!(!is_strictly_newer("0.2.0-rc1", "0.2.0"));
    }
    #[test]
    fn pre_release_b() {
        assert!(!is_strictly_newer("0.2.0", "0.2.0-rc1"));
    }
}

#[cfg(test)]
mod env_merge_tests {
    //! F4: `setup` rebuilt `.env` from a fixed allowlist and used
    //! `File::create` (which truncates). Operator-added keys such as
    //! `TRIAGE_HOME`, `TRIAGE_OWNER`, `TRIAGE_RUBRIC_PATH`, `CODEX_MODEL`,
    //! `DIFF_VIEWER`, `TRIAGE_FIXTURES_DIR`, and `DD_CALL_CENTER_TAG` were
    //! silently dropped on the next `triage-cli setup` invocation.
    //!
    //! The fix routes the writer through `merge_env_content`: read existing
    //! → replace updated keys in-place → preserve unknown keys + comments
    //! verbatim → append new keys.
    use super::merge_env_content;

    fn updates(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn merge_with_empty_existing_emits_header_and_updates() {
        let out = merge_env_content("", &updates(&[("FOO", "bar")]));
        assert!(out.starts_with("# Generated by triage-cli setup"));
        assert!(out.contains("FOO=bar"));
    }

    #[test]
    fn merge_preserves_unknown_existing_keys() {
        let existing = "# Generated by triage-cli setup\nFOO=old\nCUSTOM_KEY=preserve-me\n";
        let out = merge_env_content(existing, &updates(&[("FOO", "new")]));
        assert!(
            out.contains("CUSTOM_KEY=preserve-me"),
            "operator-added key must be preserved on setup re-run: {out}"
        );
        assert!(out.contains("FOO=new"));
        assert!(
            !out.contains("FOO=old"),
            "stale value must be replaced: {out}"
        );
    }

    #[test]
    fn merge_preserves_comments_and_blank_lines() {
        let existing = "# my custom comment\n\nFOO=old\n# another comment\nBAR=keep\n";
        let out = merge_env_content(existing, &updates(&[("FOO", "new")]));
        assert!(out.contains("# my custom comment"));
        assert!(out.contains("# another comment"));
        assert!(out.contains("BAR=keep"));
        assert!(out.contains("FOO=new"));
    }

    #[test]
    fn merge_updates_existing_key_in_place_not_at_end() {
        let existing = "FOO=old\nCUSTOM=keep\n";
        let out = merge_env_content(existing, &updates(&[("FOO", "new")]));
        let foo_pos = out.find("FOO=new").expect("FOO=new must be present");
        let custom_pos = out
            .find("CUSTOM=keep")
            .expect("CUSTOM=keep must be present");
        assert!(
            foo_pos < custom_pos,
            "in-place update must keep ordering: {out}"
        );
    }

    #[test]
    fn merge_appends_new_keys_after_existing() {
        let existing = "FOO=keep\n";
        let out = merge_env_content(existing, &updates(&[("NEW_KEY", "added")]));
        assert!(out.contains("FOO=keep"));
        assert!(out.contains("NEW_KEY=added"));
        let foo_pos = out.find("FOO=keep").unwrap();
        let new_pos = out.find("NEW_KEY=added").unwrap();
        assert!(foo_pos < new_pos, "new keys append at end: {out}");
    }

    #[test]
    fn merge_quotes_values_with_spaces_or_hash() {
        let out = merge_env_content("", &updates(&[("MSG", "hello world"), ("TAG", "x#y")]));
        assert!(
            out.contains("MSG=\"hello world\""),
            "values with spaces quoted: {out}"
        );
        assert!(out.contains("TAG=\"x#y\""), "values with # quoted: {out}");
    }

    #[test]
    fn merge_skips_empty_updates_for_new_keys() {
        // Empty value for a brand-new key is treated as "not set" — don't
        // emit it. Avoids cluttering the file with empty-key noise when an
        // optional prompt was left blank.
        let out = merge_env_content("", &updates(&[("FOO", "bar"), ("OPT", "")]));
        assert!(out.contains("FOO=bar"));
        assert!(
            !out.contains("OPT="),
            "empty update for new key should be skipped: {out}"
        );
    }
}
