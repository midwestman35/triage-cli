//! Long-running watcher: poll a Zendesk view and triage new/updated tickets.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::datadog::{DatadogClient, DatadogSource};
use crate::extract;
use crate::investigation;
use crate::models::{SiteEntry, Ticket};
use crate::pipeline::{self, InvestigateOptions, Reporter, SilentReporter};
use crate::playbook::Rubric;
use crate::zendesk::{ZendeskClient, ZendeskError};

const STATE_VERSION: u32 = 1;
pub const DEFAULT_PRUNE_CAP: usize = 1000;
pub const DEFAULT_TTL_DAYS: i64 = 30;
pub const DEFAULT_MEMBERSHIP_GRACE_DAYS: i64 = 7;

#[derive(Debug, Error)]
pub enum WatcherError {
    #[error(transparent)]
    Zendesk(#[from] ZendeskError),
    #[error(transparent)]
    Pipeline(#[from] pipeline::PipelineError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("State file {0} contains invalid JSON: {1}")]
    InvalidStateJson(PathBuf, String),
    #[error("State file {0} is not a valid watcher state object")]
    InvalidStateShape(PathBuf),
    #[error("State file {0} has version {1}; this watcher supports version {2}")]
    StateVersionMismatch(PathBuf, u32, u32),
    #[error("View {0} not found")]
    ViewNotFound(u64),
    #[error(transparent)]
    Extract(#[from] extract::ExtractError),
}

#[derive(Debug, Clone)]
pub struct WatcherOptions {
    /// `None` => tickets assigned to the authenticated user.
    pub view_id: Option<u64>,
    pub interval: u64,
    pub state_file: PathBuf,
    pub backfill_hours: f64,
    pub window_minutes: i32,
    pub levels: Vec<String>,
    pub no_logs: bool,
    pub print_notes: bool,
    pub verbose: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub triaged: std::collections::BTreeMap<String, String>,
}

impl State {
    fn empty() -> Self {
        Self {
            version: STATE_VERSION,
            triaged: Default::default(),
        }
    }
}

pub fn load_state(path: &Path) -> Result<State, WatcherError> {
    if !path.exists() {
        return Ok(State::empty());
    }
    let text = fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| WatcherError::InvalidStateJson(path.to_path_buf(), e.to_string()))?;
    let obj = value
        .as_object()
        .ok_or_else(|| WatcherError::InvalidStateShape(path.to_path_buf()))?;
    let version =
        obj.get("version")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| WatcherError::InvalidStateShape(path.to_path_buf()))? as u32;
    if version != STATE_VERSION {
        return Err(WatcherError::StateVersionMismatch(
            path.to_path_buf(),
            version,
            STATE_VERSION,
        ));
    }
    let triaged = obj
        .get("triaged")
        .and_then(|v| v.as_object())
        .ok_or_else(|| WatcherError::InvalidStateShape(path.to_path_buf()))?;
    let mut map = std::collections::BTreeMap::new();
    for (k, v) in triaged {
        if let Some(s) = v.as_str() {
            map.insert(k.clone(), s.to_string());
        }
    }
    Ok(State {
        version: STATE_VERSION,
        triaged: map,
    })
}

pub fn save_state(path: &Path, state: &State) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp_path = path.to_path_buf();
    let mut filename = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    filename.push_str(".tmp");
    tmp_path.set_file_name(filename);
    {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(serde_json::to_string_pretty(state)?.as_bytes())?;
    }
    fs::rename(&tmp_path, path)
}

pub fn should_triage(ticket: &Ticket, state: &State, backfill_cutoff: DateTime<Utc>) -> bool {
    let key = ticket.id.to_string();
    let updated = ticket.updated_at.unwrap_or(ticket.created_at);
    match state.triaged.get(&key) {
        None => updated >= backfill_cutoff,
        Some(stored) => {
            let stored_dt = DateTime::parse_from_rfc3339(stored)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            updated > stored_dt
        }
    }
}

/// Applies the count cap and TTL filter. Entries whose ticket id is in
/// `live_ids` are exempt from the TTL filter — a ticket currently in the
/// view is always kept regardless of how old its `updated_at` is, since
/// dropping it would cause the next iteration to re-triage it and write a
/// duplicate ticket folder. Unparseable timestamps are dropped with a
/// warning to stderr so the operator can repair the state file.
pub fn prune_state(
    state: State,
    max_entries: usize,
    ttl_days: i64,
    live_ids: &HashSet<String>,
) -> State {
    let cutoff = Utc::now() - chrono::Duration::days(ttl_days);
    let mut items: Vec<(String, String)> = state
        .triaged
        .into_iter()
        .filter(|(id, ts)| {
            if live_ids.contains(id) {
                return true;
            }
            match DateTime::parse_from_rfc3339(ts) {
                Ok(d) => d.with_timezone(&Utc) >= cutoff,
                Err(_) => {
                    eprintln!(
                        "watcher: dropping state entry {id:?} with unparseable timestamp {ts:?}"
                    );
                    false
                }
            }
        })
        .collect();
    items.sort_by(|a, b| b.1.cmp(&a.1));
    items.truncate(max_entries);
    State {
        version: STATE_VERSION,
        triaged: items.into_iter().collect(),
    }
}

/// Drops state entries for tickets that are no longer in the view AND whose
/// stored `updated_at` is older than `grace_days`. Entries within the grace
/// window are kept so a ticket that briefly leaves and re-enters the view
/// isn't re-triaged.
///
/// **Caveat (v1 state schema):** the grace window is measured against
/// `ticket.updated_at` (when Zendesk last changed the ticket), *not* the
/// time since the ticket left the view. A ticket that drops out of the view
/// today but was last updated 8 days ago will be evicted on the next call,
/// because the v1 state schema has no "left view at" field to anchor the
/// window on. Making the grace truly departure-relative is a v2 schema
/// change.
///
/// Unparseable timestamps are treated as expired and dropped.
pub fn prune_by_membership(mut state: State, live_ids: &HashSet<String>, grace_days: i64) -> State {
    let cutoff = Utc::now() - chrono::Duration::days(grace_days);
    state.triaged.retain(|id, ts| {
        live_ids.contains(id)
            || DateTime::parse_from_rfc3339(ts)
                .map(|d| d.with_timezone(&Utc) >= cutoff)
                .unwrap_or(false)
    });
    state
}

fn now_local_hms() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

fn emit(msg: &str) {
    eprintln!("{msg}");
}

pub async fn run_iteration(
    zd: &ZendeskClient,
    _sites: &[SiteEntry],
    mut state: State,
    opts: &WatcherOptions,
    backfill_cutoff: DateTime<Utc>,
    dd_client: Option<&dyn DatadogSource>,
    rubric: &Rubric,
) -> Result<State, WatcherError> {
    let view_ids = match opts.view_id {
        Some(id) => match zd.list_view_ticket_ids(id).await {
            Ok(ids) => ids,
            Err(ZendeskError::ViewNotFound(_)) => return Err(WatcherError::ViewNotFound(id)),
            Err(e) => {
                emit(&format!("[{}] iteration aborted: {e}", now_local_hms()));
                return Ok(state);
            }
        },
        None => match zd.list_my_ticket_ids().await {
            Ok(ids) => ids,
            Err(e) => {
                emit(&format!("[{}] iteration aborted: {e}", now_local_hms()));
                return Ok(state);
            }
        },
    };

    let live_set: HashSet<String> = view_ids.iter().map(|id| id.to_string()).collect();

    for tid in view_ids {
        let key = tid.to_string();
        let ticket = match zd.get_ticket(tid).await {
            Ok(t) => t,
            Err(e) => {
                emit(&format!(
                    "[{}] #{tid} failed: {e} (will retry)",
                    now_local_hms()
                ));
                continue;
            }
        };

        let stored = state.triaged.get(&key).cloned();
        if !should_triage(&ticket, &state, backfill_cutoff) {
            if stored.is_none() {
                // First-run silent backfill.
                state.triaged.insert(
                    key.clone(),
                    ticket.updated_at.unwrap_or(ticket.created_at).to_rfc3339(),
                );
            } else {
                emit(&format!("[{}] #{tid} unchanged", now_local_hms()));
            }
            continue;
        }

        let mut session = investigation::create_session(ticket.clone());
        let opts_inner = InvestigateOptions {
            interactive: false,
            workspace: None,
            cnc_override: None,
            site_override: None,
            anchor_override: None,
            window_minutes: opts.window_minutes,
            levels: opts.levels.clone(),
            verbose: opts.verbose,
            redact_enabled: true,
            no_llm: false,
            // Watcher mode: never overwrite another analyst's claim. A
            // soft-lock conflict surfaces as an error and the ticket is
            // skipped for this poll cycle.
            force: false,
            customer_history_override: None,
            memory_hits_override: None,
            followup_mode: false,
        };
        let reporter: Box<dyn Reporter> = Box::new(SilentReporter);
        match pipeline::investigate_one_structured(
            ticket.clone(),
            &mut session,
            dd_client,
            rubric,
            reporter.as_ref(),
            &opts_inner,
        )
        .await
        {
            Ok(outcome) => {
                let fork = outcome.report.fork_packet.commitment.fork_letter.as_str();
                let conf = outcome.report.fork_packet.commitment.confidence.as_str();
                emit(&format!(
                    "[{}] #{tid} triaged → fork={fork} confidence={conf}",
                    now_local_hms()
                ));
                if opts.verbose && !outcome.validator_warnings.is_empty() {
                    for w in &outcome.validator_warnings {
                        emit(&format!("[{}] #{tid} validator-warn: {w}", now_local_hms()));
                    }
                }
                if opts.print_notes {
                    match fs::read_to_string(&outcome.paths.fork_packet) {
                        Ok(text) => println!("{text}\n---"),
                        Err(e) => emit(&format!(
                            "[{}] #{tid} could not read FORK_PACKET.md: {e}",
                            now_local_hms()
                        )),
                    }
                }
                state.triaged.insert(
                    key,
                    ticket.updated_at.unwrap_or(ticket.created_at).to_rfc3339(),
                );
                let _ = outcome;
            }
            Err(e) => {
                emit(&format!(
                    "[{}] #{tid} failed: {e} (will retry)",
                    now_local_hms()
                ));
            }
        }
    }
    state = prune_by_membership(state, &live_set, DEFAULT_MEMBERSHIP_GRACE_DAYS);
    state = prune_state(state, DEFAULT_PRUNE_CAP, DEFAULT_TTL_DAYS, &live_set);
    Ok(state)
}

/// Main loop. Polls a view, triages new/updated tickets, sleeps, repeats.
pub async fn run_watch(opts: WatcherOptions) -> Result<(), WatcherError> {
    let sites = extract::load_site_map(Path::new("data/cnc-map.json"))?;
    let mut state = load_state(&opts.state_file)?;
    let rubric = Rubric::load()
        .map_err(|e| WatcherError::Io(std::io::Error::other(format!("rubric load failed: {e}"))))?;
    let cutoff = if opts.backfill_hours.is_finite() {
        Utc::now() - chrono::Duration::hours(opts.backfill_hours as i64)
    } else {
        chrono::DateTime::<Utc>::MIN_UTC
    };

    let mut iteration: u32 = 0;
    let interval = Duration::from_secs(opts.interval);

    let token = tokio::signal::ctrl_c();
    tokio::pin!(token);

    loop {
        iteration += 1;
        emit(&format!(
            "[{}] iteration {iteration} start (view={:?})",
            now_local_hms(),
            opts.view_id
        ));
        let zd = ZendeskClient::from_env()?;
        if opts.no_logs {
            state = run_iteration(&zd, &sites, state, &opts, cutoff, None, &rubric).await?;
        } else {
            match DatadogClient::from_env() {
                Ok(dd) => {
                    state = run_iteration(
                        &zd,
                        &sites,
                        state,
                        &opts,
                        cutoff,
                        Some(&dd as &dyn DatadogSource),
                        &rubric,
                    )
                    .await?;
                }
                Err(_) => {
                    state = run_iteration(&zd, &sites, state, &opts, cutoff, None, &rubric).await?;
                }
            }
        }
        // `run_iteration` already pruned the returned state (membership +
        // TTL with the live_set in scope). Just persist it.
        save_state(&opts.state_file, &state)?;
        emit(&format!(
            "[{}] iteration {iteration} done; sleeping {}s",
            now_local_hms(),
            opts.interval
        ));
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = &mut token => {
                emit(&format!("[{}] watcher stopped (Ctrl-C)", now_local_hms()));
                save_state(&opts.state_file, &state)?;
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(entries: &[(&str, i64)]) -> State {
        // entries: (ticket_id, age_days_ago)
        let triaged = entries
            .iter()
            .map(|(id, days_ago)| {
                let ts = (Utc::now() - chrono::Duration::days(*days_ago)).to_rfc3339();
                (id.to_string(), ts)
            })
            .collect();
        State {
            version: STATE_VERSION,
            triaged,
        }
    }

    fn live(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prune_drops_entries_older_than_ttl() {
        let state = make_state(&[("1", 10), ("2", 31), ("3", 5)]);
        let pruned = prune_state(state, 1000, 30, &live(&[]));
        assert!(
            pruned.triaged.contains_key("1"),
            "10-day-old entry should be kept"
        );
        assert!(
            !pruned.triaged.contains_key("2"),
            "31-day-old entry should be dropped"
        );
        assert!(
            pruned.triaged.contains_key("3"),
            "5-day-old entry should be kept"
        );
    }

    #[test]
    fn prune_count_cap_applied_after_ttl() {
        // 3 fresh entries, cap of 2 — oldest fresh entry should be evicted
        let state = make_state(&[("old-fresh", 20), ("newer", 5), ("newest", 1)]);
        let pruned = prune_state(state, 2, 30, &live(&[]));
        assert_eq!(pruned.triaged.len(), 2);
        assert!(
            !pruned.triaged.contains_key("old-fresh"),
            "cap should evict oldest remaining"
        );
    }

    #[test]
    fn prune_drops_unparseable_timestamps() {
        let mut state = State::empty();
        state
            .triaged
            .insert("bad".to_string(), "not-a-date".to_string());
        state
            .triaged
            .insert("good".to_string(), Utc::now().to_rfc3339());
        let pruned = prune_state(state, 1000, 30, &live(&[]));
        assert!(!pruned.triaged.contains_key("bad"));
        assert!(pruned.triaged.contains_key("good"));
    }

    #[test]
    fn prune_empty_state_is_a_noop() {
        let pruned = prune_state(State::empty(), 1000, 30, &live(&[]));
        assert!(pruned.triaged.is_empty());
    }

    #[test]
    fn prune_keeps_live_entries_past_ttl() {
        // Regression for the bug where in-view tickets dormant for >30 days
        // were being evicted from state and then re-triaged on the next
        // poll, producing duplicate ticket folders.
        let state = make_state(&[("live-stale", 60), ("dead-stale", 60)]);
        let pruned = prune_state(state, 1000, 30, &live(&["live-stale"]));
        assert!(
            pruned.triaged.contains_key("live-stale"),
            "in-view ticket must be kept regardless of age"
        );
        assert!(
            !pruned.triaged.contains_key("dead-stale"),
            "out-of-view ticket past TTL must still be dropped"
        );
    }

    #[test]
    fn prune_keeps_live_unparseable_entry() {
        // Unparseable timestamp would normally evict, but if the ticket is
        // currently in-view we keep it to avoid the re-triage duplicate.
        let mut state = State::empty();
        state
            .triaged
            .insert("live-bad".to_string(), "not-a-date".to_string());
        let pruned = prune_state(state, 1000, 30, &live(&["live-bad"]));
        assert!(pruned.triaged.contains_key("live-bad"));
    }

    #[test]
    fn membership_prune_keeps_live_ids_regardless_of_age() {
        let state = make_state(&[("live-old", 60), ("live-new", 1)]);
        let pruned = prune_by_membership(state, &live(&["live-old", "live-new"]), 7);
        assert!(
            pruned.triaged.contains_key("live-old"),
            "live IDs are always kept"
        );
        assert!(pruned.triaged.contains_key("live-new"));
    }

    #[test]
    fn membership_prune_keeps_non_live_within_grace() {
        let state = make_state(&[("gone-recent", 3), ("gone-old", 10)]);
        let pruned = prune_by_membership(state, &live(&[]), 7);
        assert!(
            pruned.triaged.contains_key("gone-recent"),
            "within grace period — keep"
        );
        assert!(
            !pruned.triaged.contains_key("gone-old"),
            "outside grace period — drop"
        );
    }

    #[test]
    fn membership_prune_drops_unparseable_non_live_timestamps() {
        let mut state = State::empty();
        state
            .triaged
            .insert("bad".to_string(), "not-a-date".to_string());
        let pruned = prune_by_membership(state, &live(&[]), 7);
        assert!(!pruned.triaged.contains_key("bad"));
    }

    #[test]
    fn membership_prune_empty_live_and_empty_state_is_noop() {
        let pruned = prune_by_membership(State::empty(), &live(&[]), 7);
        assert!(pruned.triaged.is_empty());
    }
}
