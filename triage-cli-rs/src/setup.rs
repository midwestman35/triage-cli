//! `triage-cli setup` and `triage-cli doctor` — onboarding + health check.
//!
//! `setup` is interactive: it prompts for required env vars, writes a `.env`
//! file in the working directory, and optionally rebuilds the site map. `doctor`
//! is a non-destructive read-only probe and matches the Python implementation.

use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use dialoguer::{Input, Password, Select};
use owo_colors::OwoColorize;

const ENV_PATH: &str = ".env";

pub fn doctor() -> ExitCode {
    let mut ok = true;
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
    let mut f = fs::File::create(path)?;
    writeln!(f, "# Generated by triage-cli setup")?;
    for (k, v) in entries {
        if v.is_empty() {
            continue;
        }
        let needs_quote = v.contains(' ') || v.contains('#');
        if needs_quote {
            writeln!(f, "{k}=\"{v}\"")?;
        } else {
            writeln!(f, "{k}={v}")?;
        }
    }
    Ok(())
}
