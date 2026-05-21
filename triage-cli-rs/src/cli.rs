//! Clap-derive CLI entry point. Mirrors the six Python subcommands:
//! `investigate`, `triage`, `inbox`, `watch`, `doctor`, `build-map`, plus a new
//! `setup` subcommand that replaces `python3.11 scripts/setup.py`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::{DateTime, Utc};
use clap::{Args, Parser, Subcommand};
use dialoguer::Confirm;
use owo_colors::OwoColorize;

use crate::build_map;
use crate::datadog::{DatadogClient, DatadogSource};
use crate::extract;
use crate::fixture::{self, FixtureDatadogClient, FixtureLoader};
use crate::interactive;
use crate::investigation;
use crate::pipeline::{
    self, InvestigateOptions, MetricValue, MetricsReporter, SpinnerReporter, StderrReporter,
};
use crate::playbook::Rubric;
use crate::setup;
use crate::ticket_folder;
use crate::tui;
use crate::watcher::{self, WatcherOptions};
use crate::zendesk::{ZendeskClient, ZendeskSource};

const VALID_LEVELS: &[&str] = &["error", "warn", "info", "debug"];

#[derive(Debug, Parser)]
#[command(
    name = "triage-cli",
    version,
    about = "Triage Zendesk tickets for the Carbyne APEX NG911/E911 platform."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Run a guided investigation on a Zendesk ticket.
    Investigate(InvestigateCmd),
    /// Triage a single ticket end-to-end (headless).
    Triage(TriageCmd),
    /// Launch the interactive inbox TUI.
    Inbox(InboxCmd),
    /// Poll a Zendesk view and triage new/updated tickets in a loop.
    Watch(WatchCmd),
    /// Health-check env vars, credentials, and output-dir writability.
    Doctor,
    /// Rebuild `data/cnc-map.json` from `apex-cnc-inventory.md`.
    BuildMap,
    /// Interactive first-run setup; writes `.env`.
    Setup,
    /// Run a canned demo fixture without credentials. Lists available fixtures
    /// when called without a name.
    Demo(DemoCmd),
    /// Copy data files from the current directory into `$TRIAGE_HOME`
    /// (or the platform default). Use this once after upgrading from
    /// cwd-coupled installs.
    MigrateHome(MigrateHomeCmd),
}

#[derive(Debug, Args)]
struct InvestigateCmd {
    /// Zendesk ticket ID or full URL.
    ticket: String,
    /// Pre-supplied local evidence file. Repeat for multiple.
    #[arg(long = "file")]
    files: Vec<PathBuf>,
    /// Pre-supplied pasted evidence as LABEL=TEXT. Repeat for multiple.
    #[arg(long = "paste")]
    pastes: Vec<String>,
    #[arg(long, default_value_t = false)]
    no_llm: bool,
    #[arg(long, default_value_t = false)]
    no_logs: bool,
    #[arg(long, default_value_t = 30)]
    window_minutes: i32,
    #[arg(long)]
    at: Option<String>,
    #[arg(long)]
    cnc: Option<String>,
    #[arg(long)]
    site: Option<String>,
    #[arg(long, default_value = "error,warn")]
    levels: String,
    #[arg(short, long, default_value_t = false)]
    verbose: bool,
    #[arg(long, default_value_t = false)]
    tui: bool,
    /// Overwrite a ticket folder owned by another analyst (bypasses the
    /// STATE.md soft-lock; spec § 7).
    #[arg(long, default_value_t = false)]
    force: bool,
    /// On soft-lock conflict, open the full STATE.md diff in `$DIFF_VIEWER`
    /// (fallback: `diff -u` printed to stderr).
    #[arg(long, default_value_t = false)]
    diff: bool,
    /// Load ticket/logs from a fixture directory instead of Zendesk/Datadog.
    #[arg(long)]
    fixture: Option<PathBuf>,
    /// Write a JSON run-metrics record to this path on success.
    #[arg(long)]
    metrics_out: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct DemoCmd {
    /// Name of the built-in fixture to run (e.g. `audio-drop`). Omit to list available fixtures.
    name: Option<String>,
    #[arg(short, long, default_value_t = false)]
    verbose: bool,
}

#[derive(Debug, Args)]
struct TriageCmd {
    ticket: String,
    #[arg(short, long, default_value_t = false)]
    verbose: bool,
    #[arg(long, default_value_t = false)]
    no_logs: bool,
    #[arg(long, default_value_t = false)]
    no_llm: bool,
    #[arg(long, default_value_t = 30)]
    window_minutes: i32,
    #[arg(long)]
    at: Option<String>,
    #[arg(long)]
    cnc: Option<String>,
    #[arg(long)]
    site: Option<String>,
    #[arg(long, default_value = "error,warn")]
    levels: String,
    #[arg(long, default_value_t = false)]
    tui: bool,
    /// Overwrite a ticket folder owned by another analyst (bypasses the
    /// STATE.md soft-lock; spec § 7).
    #[arg(long, default_value_t = false)]
    force: bool,
    /// On soft-lock conflict, open the full STATE.md diff in `$DIFF_VIEWER`
    /// (fallback: `diff -u` printed to stderr).
    #[arg(long, default_value_t = false)]
    diff: bool,
    /// Load ticket/logs from a fixture directory instead of Zendesk/Datadog.
    #[arg(long)]
    fixture: Option<PathBuf>,
    /// Write a JSON run-metrics record to this path on success.
    #[arg(long)]
    metrics_out: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct InboxCmd {
    /// View ID or named queue (e.g. "unassigned"). Default: your assigned tickets.
    #[arg(long)]
    view: Option<String>,
    #[arg(long, default_value_t = 60)]
    poll: u64,
    #[arg(long, default_value = "0")]
    backfill: String,
    #[arg(long, default_value_t = 15)]
    window_minutes: i32,
    #[arg(long, default_value = "error,warn")]
    levels: String,
    #[arg(long, default_value_t = false)]
    no_logs: bool,
    #[arg(short, long, default_value_t = false)]
    verbose: bool,
}

#[derive(Debug, Args)]
struct WatchCmd {
    #[arg(long)]
    view: u64,
    #[arg(long, default_value_t = 300)]
    interval: u64,
    #[arg(long)]
    state_file: Option<PathBuf>,
    #[arg(long, default_value = "24h")]
    backfill: String,
    #[arg(long, default_value_t = 30)]
    window_minutes: i32,
    #[arg(long, default_value = "error,warn")]
    levels: String,
    #[arg(long, default_value_t = false)]
    no_logs: bool,
    #[arg(long, default_value_t = false)]
    print_notes: bool,
    #[arg(short, long, default_value_t = false)]
    verbose: bool,
}

#[derive(Debug, Args)]
struct MigrateHomeCmd {
    /// Overwrite files that already exist at the destination instead of
    /// skipping them. Without this flag, existing files are kept and a
    /// "kept existing <name>" notice is printed to stderr.
    #[arg(long, default_value_t = false)]
    force: bool,
}

pub fn run() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Doctor => async_run(setup::doctor),
        Cmd::Setup => setup::setup(),
        Cmd::BuildMap => cmd_build_map(),
        Cmd::MigrateHome(c) => cmd_migrate_home(c),
        Cmd::Triage(c) => async_run(|| cmd_triage(c)),
        Cmd::Investigate(c) => async_run(|| cmd_investigate(c)),
        Cmd::Watch(c) => async_run(|| cmd_watch(c)),
        Cmd::Inbox(c) => async_run(|| cmd_inbox(c)),
        Cmd::Demo(c) => async_run(|| cmd_demo(c)),
    }
}

fn async_run<F, Fut>(f: F) -> ExitCode
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ExitCode>,
{
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            die(&format!("failed to start tokio runtime: {e}"));
        }
    };
    rt.block_on(f())
}

fn die(msg: &str) -> ! {
    eprintln!("{}: {}", "Error".red().bold(), msg);
    std::process::exit(1);
}

fn parse_at(s: &str) -> DateTime<Utc> {
    let normalized = s.replace('Z', "+00:00");
    match DateTime::parse_from_rfc3339(&normalized) {
        Ok(d) => d.with_timezone(&Utc),
        Err(e) => die(&format!("--at must be ISO 8601 (got {s:?}): {e}")),
    }
}

fn parse_levels(s: &str) -> Vec<String> {
    let parts: Vec<String> = s
        .split(',')
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        die("--levels must be a non-empty comma-separated list");
    }
    let invalid: Vec<&str> = parts
        .iter()
        .filter(|p| !VALID_LEVELS.contains(&p.as_str()))
        .map(String::as_str)
        .collect();
    if !invalid.is_empty() {
        die(&format!(
            "Invalid log levels: {invalid:?}. Valid: {VALID_LEVELS:?}"
        ));
    }
    parts
}

fn parse_backfill(value: &str) -> f64 {
    let s = value.trim().to_ascii_lowercase();
    if s == "inf" {
        return f64::INFINITY;
    }
    if s == "0" {
        return 0.0;
    }
    if let Some(prefix) = s.strip_suffix('h') {
        if let Ok(n) = prefix.parse::<u64>() {
            return n as f64;
        }
    }
    if let Some(prefix) = s.strip_suffix('d') {
        if let Ok(n) = prefix.parse::<u64>() {
            return (n * 24) as f64;
        }
    }
    die(&format!(
        "--backfill must be 'inf', '0', 'Nh', or 'Nd' (got {value:?})"
    ));
}

fn parse_paste(value: &str) -> (String, String) {
    let (label, text) = value
        .split_once('=')
        .unwrap_or_else(|| die("--paste must be LABEL=TEXT"));
    if label.trim().is_empty() {
        die("--paste must be LABEL=TEXT");
    }
    (label.trim().to_string(), text.to_string())
}

fn resolve_view(view: Option<&str>) -> (Option<u64>, String) {
    let Some(v) = view else {
        return (None, "me".into());
    };
    if let Ok(id) = v.parse::<u64>() {
        return (Some(id), v.to_string());
    }
    let views_path = crate::paths::triage_home().join("data/views.json");
    if views_path.exists() {
        if let Ok(text) = std::fs::read_to_string(&views_path) {
            if let Ok(map) =
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&text)
            {
                if let Some(id) = map.get(v).and_then(|val| val.as_u64()) {
                    return (Some(id), v.to_string());
                }
            }
        }
    }
    die(&format!(
        "Unknown view {v:?}. Use a numeric ID or a name from data/views.json."
    ));
}

fn cmd_build_map() -> ExitCode {
    build_map::run()
}

fn cmd_migrate_home(c: MigrateHomeCmd) -> ExitCode {
    let src = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => die(&format!("migrate-home: could not determine cwd: {e}")),
    };
    let dest = crate::paths::migrate_home_dest();
    match crate::paths::migrate_home(&src, &dest, c.force) {
        Ok(path) => {
            eprintln!("Done. You can now run triage-cli from any directory.");
            eprintln!("Files migrated to: {}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => die(&format!("migrate-home failed: {e}")),
    }
}

async fn cmd_triage(c: TriageCmd) -> ExitCode {
    let at_dt = c.at.as_deref().map(parse_at);
    let levels = parse_levels(&c.levels);

    if c.tui {
        die("`--tui` was removed in the v1 reframe; the inbox TUI \
             (`triage-cli inbox`) is now the only TUI surface. Run \
             without `--tui` to produce the ticket folder.");
    }

    // Fixture mode: bypass Zendesk and Datadog entirely.
    if let Some(fixture_path) = c.fixture {
        let loader = match FixtureLoader::new(&fixture_path) {
            Ok(l) => l,
            Err(e) => die(&format!("fixture: {e}")),
        };
        let ticket = match loader.load_ticket() {
            Ok(t) => t,
            Err(e) => die(&format!("fixture ticket: {e}")),
        };
        let logs = match loader.load_datadog_logs() {
            Ok(l) => l,
            Err(e) => die(&format!("fixture logs: {e}")),
        };
        let memory_hits = match loader.load_memory_hits() {
            Ok(h) => h,
            Err(e) => die(&format!("fixture memory-hits: {e}")),
        };
        if c.verbose {
            eprintln!(
                "Fixture mode: ticket #{} — {} — {} log line(s) — {} memory hit(s)",
                ticket.id,
                ticket.subject,
                logs.len(),
                memory_hits.len()
            );
        }
        let mut session = investigation::create_session(ticket.clone());
        let fixture_dd = FixtureDatadogClient::new(logs);
        let opts = InvestigateOptions {
            interactive: false,
            workspace: None,
            cnc_override: c.cnc,
            site_override: c.site,
            anchor_override: at_dt,
            window_minutes: c.window_minutes,
            levels,
            verbose: c.verbose,
            redact_enabled: true,
            no_llm: c.no_llm,
            force: c.force,
            customer_history_override: None,
            memory_hits_override: Some(memory_hits),
            followup_mode: false,
            tickets_root: None,
        };
        let rubric = load_rubric_or_die();
        let reporter = MetricsReporter::new(Box::new(StderrReporter { verbose: c.verbose }));
        let outcome = match pipeline::investigate_one_structured(
            ticket.clone(),
            &mut session,
            None,
            Some(&fixture_dd as &dyn DatadogSource),
            &rubric,
            &reporter,
            &opts,
        )
        .await
        {
            Ok(o) => o,
            Err(e) => return handle_pipeline_error(e, c.diff),
        };
        print_fork_packet_to_stdout(&outcome.paths.fork_packet);
        surface_validator_warnings(&outcome.validator_warnings);
        if c.verbose {
            eprintln!(
                "Ticket folder: {} (fork={}, confidence={})",
                outcome.paths.folder.display(),
                outcome.report.fork_packet.commitment.fork_letter.as_str(),
                outcome.report.fork_packet.commitment.confidence.as_str(),
            );
        }
        if let Some(path) = c.metrics_out {
            write_metrics_file(&path, ticket.id, &reporter, &outcome);
        }
        return ExitCode::SUCCESS;
    }

    let ticket_id = match extract::parse_ticket_id(&c.ticket) {
        Ok(id) => id,
        Err(e) => die(&e.to_string()),
    };

    let zd = match ZendeskClient::from_env() {
        Ok(c) => c,
        Err(e) => die(&e.to_string()),
    };
    let ticket = match pipeline::spinner(
        &format!("Fetching ticket #{ticket_id}"),
        true,
        zd.get_ticket(ticket_id),
    )
    .await
    {
        Ok(t) => t,
        Err(e) => die(&e.to_string()),
    };
    if c.verbose {
        eprintln!(
            "Fetched ticket #{} — subject: {}",
            ticket.id, ticket.subject
        );
    }

    let mut session = investigation::create_session(ticket.clone());
    let dd = if c.no_logs {
        None
    } else {
        match DatadogClient::from_env() {
            Ok(d) => Some(d),
            Err(_) => {
                if c.verbose {
                    eprintln!("Datadog not configured — skipping logs");
                }
                None
            }
        }
    };

    let opts = InvestigateOptions {
        interactive: false,
        workspace: None,
        cnc_override: c.cnc,
        site_override: c.site,
        anchor_override: at_dt,
        window_minutes: c.window_minutes,
        levels,
        verbose: c.verbose,
        redact_enabled: true,
        no_llm: c.no_llm,
        force: c.force,
        customer_history_override: None,
        memory_hits_override: None,
        followup_mode: false,
        tickets_root: None,
    };

    let rubric = load_rubric_or_die();
    let reporter = MetricsReporter::new(Box::new(StderrReporter { verbose: c.verbose }));
    let outcome = match pipeline::investigate_one_structured(
        ticket.clone(),
        &mut session,
        Some(&zd as &dyn ZendeskSource),
        dd.as_ref().map(|d| d as &dyn DatadogSource),
        &rubric,
        &reporter,
        &opts,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => return handle_pipeline_error(e, c.diff),
    };
    print_fork_packet_to_stdout(&outcome.paths.fork_packet);
    surface_validator_warnings(&outcome.validator_warnings);
    if c.verbose {
        eprintln!(
            "Ticket folder: {} (fork={}, confidence={})",
            outcome.paths.folder.display(),
            outcome.report.fork_packet.commitment.fork_letter.as_str(),
            outcome.report.fork_packet.commitment.confidence.as_str(),
        );
    }
    if let Some(path) = c.metrics_out {
        write_metrics_file(&path, ticket.id, &reporter, &outcome);
    }
    ExitCode::SUCCESS
}

async fn cmd_investigate(c: InvestigateCmd) -> ExitCode {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        die("investigate requires an interactive terminal. Use 'triage' for headless runs.");
    }
    let ticket_id = match extract::parse_ticket_id(&c.ticket) {
        Ok(id) => id,
        Err(e) => die(&e.to_string()),
    };
    let parsed_pastes: Vec<(String, String)> = c.pastes.iter().map(|p| parse_paste(p)).collect();
    let at_dt = c.at.as_deref().map(parse_at);
    let levels = parse_levels(&c.levels);
    for path in &c.files {
        if !path.exists() {
            die(&format!(
                "Local evidence file not found: {}",
                path.display()
            ));
        }
    }

    // Pre-flight: if a completed investigation already exists on disk, warn and
    // prompt before burning a network+LLM round-trip. Skipped when --force is
    // set, since the caller has explicitly opted in to overwriting.
    if !c.force {
        let state_path = ticket_folder::tickets_root()
            .join(ticket_id.to_string())
            .join("STATE.md");
        if let Some(existing) = ticket_folder::read_existing_state(&state_path) {
            let fork = existing.fork.as_deref().unwrap_or("?");
            let confidence = existing.confidence.as_deref().unwrap_or("?");
            let owner = existing.owner.as_deref().unwrap_or("unknown");
            let folder = ticket_folder::tickets_root().join(ticket_id.to_string());
            eprintln!(
                "{} ZD-{ticket_id} already has an investigation.\n   Fork: {}  Confidence: {}  Owner: {}\n   Folder: {}",
                "⚠".yellow().bold(),
                fork.bold(),
                confidence,
                owner.bold(),
                folder.display(),
            );
            if !Confirm::new()
                .with_prompt("Re-investigate anyway?")
                .default(false)
                .interact()
                .unwrap_or(false)
            {
                return ExitCode::SUCCESS;
            }
        }
    }

    let zd = match ZendeskClient::from_env() {
        Ok(z) => z,
        Err(e) => die(&e.to_string()),
    };
    let ticket = match pipeline::spinner(
        &format!("Fetching ticket #{ticket_id}"),
        true,
        zd.get_ticket(ticket_id),
    )
    .await
    {
        Ok(t) => t,
        Err(e) => die(&e.to_string()),
    };
    if c.verbose {
        eprintln!(
            "Fetched ticket #{} — subject: {}",
            ticket.id, ticket.subject
        );
    }
    let scratch_root_buf = crate::paths::triage_home().join("scratch");
    let workspace = match interactive::ensure_workspace(scratch_root_buf.as_path(), ticket.id) {
        Ok(w) => w,
        Err(e) => die(&format!("workspace: {e}")),
    };

    eprintln!(
        "ZD-{} · {} · {} attachment(s) · {} comment(s)",
        ticket.id,
        ticket.requester_org.as_deref().unwrap_or("(no org)"),
        ticket
            .comments
            .iter()
            .map(|c| c.attachments.len())
            .sum::<usize>(),
        ticket.comments.len()
    );

    let downloaded =
        interactive::download_attachments(&ticket, &zd, &workspace, 150 * 1024 * 1024).await;
    let local_files = interactive::prompt_drop_and_wait(&workspace);
    eprintln!(
        "{}",
        interactive::summarize_workspace(&workspace, &local_files, &downloaded)
    );

    let mut session = investigation::create_session(ticket.clone());
    for lf in local_files {
        session.evidence.local_files.push(lf);
    }
    for d in downloaded {
        session.evidence.attachments.push(d);
    }
    for path in &c.files {
        if let Err(e) = investigation::add_local_file(&mut session, path) {
            die(&format!("Could not read --file {}: {e}", path.display()));
        }
    }
    for (label, text) in parsed_pastes {
        investigation::add_pasted_evidence(&mut session, &label, &text);
    }

    let dd = if c.no_logs {
        None
    } else {
        DatadogClient::from_env().ok()
    };

    // Fixture mode: also load fixture logs if --fixture is set.
    let fixture_dd = if let Some(ref fixture_path) = c.fixture {
        match FixtureLoader::new(fixture_path) {
            Ok(loader) => match loader.load_datadog_logs() {
                Ok(logs) => Some(FixtureDatadogClient::new(logs)),
                Err(e) => die(&format!("fixture logs: {e}")),
            },
            Err(e) => die(&format!("fixture: {e}")),
        }
    } else {
        None
    };
    let fixture_memory = if let Some(ref fixture_path) = c.fixture {
        match FixtureLoader::new(fixture_path) {
            Ok(loader) => match loader.load_memory_hits() {
                Ok(hits) => Some(hits),
                Err(e) => die(&format!("fixture memory-hits: {e}")),
            },
            Err(e) => die(&format!("fixture: {e}")),
        }
    } else {
        None
    };

    let opts = InvestigateOptions {
        interactive: true,
        workspace: Some(workspace.root.clone()),
        cnc_override: c.cnc,
        site_override: c.site,
        anchor_override: at_dt,
        window_minutes: c.window_minutes,
        levels,
        verbose: c.verbose,
        redact_enabled: true,
        no_llm: c.no_llm,
        force: c.force,
        customer_history_override: None,
        memory_hits_override: fixture_memory,
        followup_mode: false,
        tickets_root: None,
    };

    if c.tui {
        die("`--tui` was removed in the v1 reframe; the inbox TUI \
             (`triage-cli inbox`) is now the only TUI surface. Run \
             without `--tui` to produce the ticket folder.");
    }
    let rubric = load_rubric_or_die();
    let reporter = MetricsReporter::new(Box::new(SpinnerReporter::new(Box::new(StderrReporter {
        verbose: c.verbose,
    }))));
    let effective_dd: Option<&dyn DatadogSource> = fixture_dd
        .as_ref()
        .map(|d| d as &dyn DatadogSource)
        .or_else(|| dd.as_ref().map(|d| d as &dyn DatadogSource));
    let outcome = match pipeline::investigate_one_structured(
        ticket.clone(),
        &mut session,
        Some(&zd as &dyn ZendeskSource),
        effective_dd,
        &rubric,
        &reporter,
        &opts,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => return handle_pipeline_error(e, c.diff),
    };
    print_fork_packet_to_stdout(&outcome.paths.fork_packet);
    surface_validator_warnings(&outcome.validator_warnings);
    eprintln!("Ticket folder ready: {}", outcome.paths.folder.display());
    if let Some(path) = c.metrics_out {
        write_metrics_file(&path, ticket.id, &reporter, &outcome);
    }
    ExitCode::SUCCESS
}

async fn cmd_demo(c: DemoCmd) -> ExitCode {
    let Some(name) = c.name else {
        eprintln!("Available demo fixtures:");
        let root = fixture::fixtures_root();
        for &n in fixture::NAMED_FIXTURES {
            let path = root.join(n);
            let mark = if path.is_dir() { "✓" } else { "✗" };
            eprintln!("  {mark} {n}");
        }
        eprintln!();
        eprintln!("Usage: triage-cli demo <name>");
        eprintln!("       triage-cli triage --fixture <path> [--no-llm]");
        return ExitCode::SUCCESS;
    };

    let fixture_path = fixture::resolve_named(&name);
    let loader = match FixtureLoader::new(&fixture_path) {
        Ok(l) => l,
        Err(e) => die(&format!("demo fixture '{name}': {e}")),
    };
    let ticket = match loader.load_ticket() {
        Ok(t) => t,
        Err(e) => die(&format!("demo fixture '{name}' ticket: {e}")),
    };
    let logs = match loader.load_datadog_logs() {
        Ok(l) => l,
        Err(e) => die(&format!("demo fixture '{name}' logs: {e}")),
    };
    let memory_hits = match loader.load_memory_hits() {
        Ok(h) => h,
        Err(e) => die(&format!("demo fixture '{name}' memory-hits: {e}")),
    };

    eprintln!(
        "Demo: fixture '{name}' — ticket #{} «{}»",
        ticket.id, ticket.subject
    );
    if c.verbose {
        eprintln!(
            "  {} log line(s), {} memory hit(s)",
            logs.len(),
            memory_hits.len()
        );
    }

    let mut session = investigation::create_session(ticket.clone());
    let fixture_dd = FixtureDatadogClient::new(logs);
    let opts = InvestigateOptions {
        interactive: false,
        workspace: None,
        cnc_override: None,
        site_override: None,
        anchor_override: None,
        window_minutes: 30,
        levels: vec!["error".into(), "warn".into(), "info".into()],
        verbose: c.verbose,
        redact_enabled: true,
        no_llm: true,
        force: false,
        customer_history_override: None,
        memory_hits_override: Some(memory_hits),
        followup_mode: false,
        tickets_root: None,
    };
    let rubric = load_rubric_or_die();
    let reporter = StderrReporter { verbose: c.verbose };
    let outcome = match pipeline::investigate_one_structured(
        ticket,
        &mut session,
        None,
        Some(&fixture_dd as &dyn DatadogSource),
        &rubric,
        &reporter,
        &opts,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            eprintln!("Error: {e}");
            return ExitCode::from(1);
        }
    };
    print_fork_packet_to_stdout(&outcome.paths.fork_packet);
    surface_validator_warnings(&outcome.validator_warnings);
    eprintln!(
        "Demo complete. Ticket folder: {}",
        outcome.paths.folder.display()
    );
    ExitCode::SUCCESS
}

fn load_rubric_or_die() -> Rubric {
    match Rubric::load() {
        Ok(r) => r,
        Err(e) => die(&format!("failed to load fork rubric: {e}")),
    }
}

/// Build and write the JSON metrics record to `path`. Errors are printed to
/// stderr but do not affect the exit code — metrics emission is best-effort.
fn write_metrics_file(
    path: &Path,
    ticket_id: u64,
    reporter: &MetricsReporter,
    outcome: &pipeline::StructuredInvestigation,
) {
    let phases = reporter.phase_timings();
    let named = reporter.named_metrics();

    // Collect evidence counts.
    let mut evidence_counts = serde_json::Map::new();
    let mut llm_obj = serde_json::Map::new();
    for (key, val) in &named {
        let jval = match val {
            MetricValue::Int(n) => serde_json::Value::Number(serde_json::Number::from(*n)),
            MetricValue::Float(f) => serde_json::json!(f),
            MetricValue::Bool(b) => serde_json::Value::Bool(*b),
            MetricValue::Str(s) => serde_json::Value::String(s.clone()),
        };
        if let Some(suffix) = key.strip_prefix("evidence.") {
            evidence_counts.insert(suffix.to_string(), jval);
        } else if let Some(suffix) = key.strip_prefix("llm.") {
            llm_obj.insert(suffix.to_string(), jval);
        }
    }

    let phases_json: serde_json::Map<String, serde_json::Value> = phases
        .into_iter()
        .map(|(k, v)| {
            let rounded = (v * 1000.0).round() / 1000.0;
            (k, serde_json::json!(rounded))
        })
        .collect();

    let record = serde_json::json!({
        "ticket_id": ticket_id,
        "phases": phases_json,
        "evidence_counts": evidence_counts,
        "llm": llm_obj,
        "validator_warnings": outcome.validator_warnings,
        "fork": outcome.report.fork_packet.commitment.fork_letter.as_str(),
        "confidence": outcome.report.fork_packet.commitment.confidence.as_str(),
        "exit_code": 0,
    });

    let text = match serde_json::to_string_pretty(&record) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "{}: could not serialize metrics: {e}",
                "warning".yellow().bold()
            );
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    if let Err(e) = std::fs::write(path, &text) {
        eprintln!(
            "{}: could not write metrics to {}: {e}",
            "warning".yellow().bold(),
            path.display()
        );
    } else {
        eprintln!("Metrics written to {}", path.display());
    }
}

fn print_fork_packet_to_stdout(path: &Path) {
    match std::fs::read_to_string(path) {
        Ok(text) => print!("{text}"),
        Err(e) => die(&format!(
            "could not read FORK_PACKET.md at {}: {e}",
            path.display()
        )),
    }
}

fn surface_validator_warnings(warnings: &[String]) {
    if warnings.is_empty() {
        return;
    }
    eprintln!("{}", "Validator soft-warnings (accepted):".yellow().bold());
    for w in warnings {
        eprintln!("  · {w}");
    }
}

/// Convert a `PipelineError` to an `ExitCode`. The only case that gets
/// special treatment is `SoftLockConflict` (spec § 7, decision 3): we print
/// a summarized field-level diff to stderr, optionally invoke the full diff
/// viewer when `--diff` is set, and exit non-zero without dying via panic.
fn handle_pipeline_error(e: pipeline::PipelineError, open_full_diff: bool) -> ExitCode {
    use crate::ticket_folder::TicketFolderError;
    if let pipeline::PipelineError::TicketFolder(TicketFolderError::SoftLockConflict {
        existing_owner,
        current_owner,
        summary,
        state_path,
        new_state_content,
    }) = &e
    {
        print_soft_lock_summary(existing_owner, current_owner, summary);
        if open_full_diff {
            if let Err(diff_err) = show_full_state_diff(state_path, new_state_content) {
                eprintln!(
                    "{}: could not produce full diff: {}",
                    "warning".yellow().bold(),
                    diff_err
                );
            }
        }
        return ExitCode::from(2);
    }
    eprintln!("{}: {}", "Error".red().bold(), e);
    ExitCode::from(1)
}

fn print_soft_lock_summary(
    existing_owner: &str,
    current_owner: &str,
    summary: &[(String, String, String)],
) {
    eprintln!(
        "{} Ticket folder is owned by {}; you are {}.",
        "⚠".yellow().bold(),
        existing_owner.bold(),
        current_owner.bold(),
    );
    if summary.is_empty() {
        eprintln!("  (No structured-field differences detected — only the owner has changed.)");
    } else {
        eprintln!("Conflicting fields:");
        let label_width = summary.iter().map(|(k, _, _)| k.len()).max().unwrap_or(0);
        for (field, old, new) in summary {
            eprintln!(
                "  {:width$}  {} → {}",
                format!("{field}:"),
                old,
                new,
                width = label_width + 1,
            );
        }
    }
    eprintln!("Run with `--diff` to open the full diff in $DIFF_VIEWER (fallback `diff -u`).");
    eprintln!("Re-run with `--force` to overwrite.");
}

/// Open the full STATE.md diff: existing content on disk vs. the new
/// rendered content that would have been written. Honors `$DIFF_VIEWER`
/// (spawned with two filename arguments); falls back to `diff -u` printed
/// to stderr.
fn show_full_state_diff(existing_path: &Path, new_content: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::process::Command;

    // Stage the "new" content in a temp file so external viewers can read it.
    let mut new_tmp = tempfile::NamedTempFile::new()?;
    new_tmp.write_all(new_content.as_bytes())?;
    new_tmp.flush()?;
    let new_path = new_tmp.path();

    if let Ok(viewer) = std::env::var("DIFF_VIEWER") {
        let trimmed = viewer.trim();
        if !trimmed.is_empty() {
            // Split the user's $DIFF_VIEWER on shell-style word boundaries.
            // `shlex::split` handles single/double-quoted args and escapes
            // exactly like POSIX `sh` would, but cross-platform and without
            // ever spawning a real shell. The file-path args are passed via
            // `Command::args`, so they never go through a parser at all.
            let parts = shlex::split(trimmed)
                .ok_or_else(|| std::io::Error::other("DIFF_VIEWER has unbalanced quoting"))?;
            let (cmd, args) = parts
                .split_first()
                .ok_or_else(|| std::io::Error::other("DIFF_VIEWER is empty after parsing"))?;
            let status = Command::new(cmd)
                .args(args)
                .arg(existing_path)
                .arg(new_path)
                .status()?;
            if !status.success() {
                eprintln!(
                    "{}: $DIFF_VIEWER exited with status {}",
                    "warning".yellow().bold(),
                    status
                );
            }
            return Ok(());
        }
    }

    // Fallback: in-process unified diff via the `similar` crate. This used
    // to shell out to `/usr/bin/diff -u`, which doesn't exist on Windows.
    let existing_bytes = std::fs::read_to_string(existing_path)?;
    let new_bytes = std::fs::read_to_string(new_path)?;
    let diff = similar::TextDiff::from_lines(&existing_bytes, &new_bytes)
        .unified_diff()
        .header("STATE.md (existing)", "STATE.md (new)")
        .to_string();
    eprintln!("{}", diff);
    Ok(())
}

async fn cmd_watch(c: WatchCmd) -> ExitCode {
    let levels = parse_levels(&c.levels);
    let backfill_hours = parse_backfill(&c.backfill);
    let state_file = c.state_file.unwrap_or_else(|| {
        crate::paths::triage_home().join(format!("data/watcher-state-{}.json", c.view))
    });
    let opts = WatcherOptions {
        view_id: Some(c.view),
        interval: c.interval,
        state_file,
        backfill_hours,
        window_minutes: c.window_minutes,
        levels,
        no_logs: c.no_logs,
        print_notes: c.print_notes,
        verbose: c.verbose,
    };
    match watcher::run_watch(opts).await {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => die(&e.to_string()),
    }
}

async fn cmd_inbox(c: InboxCmd) -> ExitCode {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        die("inbox requires an interactive terminal. Use `watch` for headless runs.");
    }
    let (view_id, view_key) = resolve_view(c.view.as_deref());
    let backfill_hours = parse_backfill(&c.backfill);
    let levels = parse_levels(&c.levels);
    let state_file =
        crate::paths::triage_home().join(format!("data/watcher-state-{view_key}.json"));
    let opts = WatcherOptions {
        view_id,
        interval: c.poll,
        state_file,
        backfill_hours,
        window_minutes: c.window_minutes,
        levels,
        no_logs: c.no_logs,
        print_notes: false,
        verbose: c.verbose,
    };
    match tui::run_inbox(opts).await {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => die(&e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{error::ErrorKind, CommandFactory};

    #[test]
    fn investigate_help_does_not_advertise_legacy_save_or_triage_notes() {
        let mut command = Cli::command();
        let investigate = command
            .find_subcommand_mut("investigate")
            .expect("investigate subcommand exists");
        let help = investigate.render_long_help().to_string();

        assert!(
            !help.contains("triage-notes"),
            "investigate help should not mention legacy triage-notes output:\n{help}"
        );
        assert!(
            !help.contains("markdown/JSON"),
            "investigate help should not mention legacy markdown/JSON sidecars:\n{help}"
        );
        assert!(
            !help.contains("--save"),
            "investigate help should not advertise removed --save flag:\n{help}"
        );
    }

    #[test]
    fn investigate_rejects_legacy_save_flag() {
        let err = Cli::try_parse_from(["triage-cli", "investigate", "12345", "--save"])
            .expect_err("legacy --save should be rejected");

        assert_eq!(err.kind(), ErrorKind::UnknownArgument);
        assert!(
            err.to_string().contains("--save"),
            "clap error should identify the rejected flag: {err}"
        );
    }
}
