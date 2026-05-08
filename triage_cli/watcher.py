"""Long-running watcher: poll a Zendesk view and triage new/updated tickets.

Public surface:
    WatcherOptions       - frozen dataclass of run-time knobs
    load_state           - read a watcher state file (or default)
    save_state           - atomic write of a state dict
    should_triage        - pure decider: (ticket, state, cutoff) -> bool
    prune_state          - bounded-growth helper; keeps N most-recent entries
    run_iteration        - single poll-and-triage pass over a view
    run_watch            - the main loop; sleeps interval seconds between iterations
"""
from __future__ import annotations

import json
import logging
import math
import os
import sys
import time
from collections.abc import Callable
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

from triage_cli import extract, pipeline, render
from triage_cli.datadog import DatadogClient
from triage_cli.models import SiteEntry, Ticket, TriageReport
from triage_cli.zendesk import ZendeskClient

logger = logging.getLogger(__name__)

STATE_VERSION = 1
DEFAULT_PRUNE_CAP = 1000


@dataclass(frozen=True)
class WatcherOptions:
    view_id: int | None  # None = tickets assigned to the authenticated user
    interval: int
    state_file: Path
    backfill_hours: float
    window_minutes: int
    levels: list[str]
    no_logs: bool
    print_notes: bool
    verbose: bool


State = dict[str, Any]


def _empty_state() -> State:
    return {"version": STATE_VERSION, "triaged": {}}


def load_state(path: Path) -> State:
    """Load a watcher state file. Returns the empty default when missing.

    Raises RuntimeError on a version we don't understand.
    """
    if not path.exists():
        return _empty_state()
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        raise RuntimeError(f"State file {path} contains invalid JSON: {e}") from e
    if not isinstance(raw, dict) or "version" not in raw or "triaged" not in raw:
        raise RuntimeError(f"State file {path} is not a valid watcher state object")
    version = raw["version"]
    if version != STATE_VERSION:
        raise RuntimeError(
            f"State file {path} has version {version}; this watcher supports "
            f"version {STATE_VERSION}"
        )
    triaged = raw["triaged"]
    if not isinstance(triaged, dict):
        raise RuntimeError(f"State file {path} 'triaged' must be an object")
    return {"version": STATE_VERSION, "triaged": dict(triaged)}


def save_state(path: Path, state: State) -> None:
    """Atomically write the state file (tempfile + os.replace)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.parent / (path.name + ".tmp")
    tmp.write_text(json.dumps(state, indent=2, sort_keys=True), encoding="utf-8")
    os.replace(tmp, path)


def should_triage(ticket: Ticket, state: State, backfill_cutoff: datetime) -> bool:
    """Decide whether to triage a ticket given current state and the cutoff.

    Rules:
      - If the ticket's updated_at is older than the cutoff and we have no
        record of it, return False (it'll be silently marked as seen by
        run_iteration).
      - If state has the same updated_at we already triaged this version.
      - If state has an older updated_at OR no entry at all (and we're past
        the cutoff check), triage.
    """
    triaged = state.get("triaged") or {}
    key = str(ticket.id)
    stored = triaged.get(key)
    if stored is None:
        return ticket.updated_at >= backfill_cutoff
    try:
        stored_dt = datetime.fromisoformat(stored)
    except ValueError:
        # Corrupt entry — re-triage to recover.
        logger.warning("watcher: corrupt timestamp for ticket %s in state: %r", key, stored)
        return True
    if stored_dt.tzinfo is None:
        stored_dt = stored_dt.replace(tzinfo=UTC)
    return ticket.updated_at > stored_dt


def prune_state(state: State, max_entries: int = DEFAULT_PRUNE_CAP) -> State:
    """Keep at most max_entries triaged entries, dropping the oldest by timestamp."""
    triaged = state.get("triaged") or {}
    if len(triaged) <= max_entries:
        return {"version": STATE_VERSION, "triaged": dict(triaged)}
    items = sorted(triaged.items(), key=lambda kv: kv[1], reverse=True)
    kept = dict(items[:max_entries])
    return {"version": STATE_VERSION, "triaged": kept}


def _now_local_hms() -> str:
    return datetime.now().strftime("%H:%M:%S")


def _emit(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def run_iteration(
    zd: ZendeskClient,
    sites: list[SiteEntry],
    state: State,
    opts: WatcherOptions,
    backfill_cutoff: datetime,
    dd_client: DatadogClient | None,
    *,
    on_view_listed: Callable[[list[int]], None] | None = None,
    on_progress: Callable[[int, str], None] | None = None,
    on_complete: Callable[[TriageReport], None] | None = None,
    on_failure: Callable[[int, str], None] | None = None,
) -> State:
    """Run one poll-and-triage pass over the view. Returns the updated state."""
    triaged_map: dict[str, str] = dict(state.get("triaged") or {})
    new_state: State = {"version": STATE_VERSION, "triaged": triaged_map}

    try:
        if opts.view_id is None:
            view_ids = zd.list_my_ticket_ids()
        else:
            view_ids = zd.list_view_ticket_ids(opts.view_id)
    except RuntimeError as e:
        # View-not-found is a permanent config error; let it propagate so
        # the CLI exits with a clear message instead of spinning forever.
        if opts.view_id is not None and str(e).startswith(f"View {opts.view_id} not found"):
            raise
        _emit(f"[{_now_local_hms()}] iteration aborted: {e}")
        return new_state

    if on_view_listed is not None:
        on_view_listed(view_ids)

    for tid in view_ids:
        key = str(tid)
        try:
            ticket = zd.get_ticket(tid)
        except (RuntimeError, ValueError) as e:
            _emit(f"[{_now_local_hms()}] #{tid} failed: {e} (will retry)")
            if on_failure is not None:
                on_failure(tid, str(e))
            continue

        stored = triaged_map.get(key)
        if not should_triage(ticket, new_state, backfill_cutoff):
            if stored is None:
                # First-run silent backfill: mark as seen, no note.
                triaged_map[key] = ticket.updated_at.isoformat()
            else:
                _emit(f"[{_now_local_hms()}] #{tid} unchanged")
            continue

        site_entry, _strategy = pipeline.resolve_site(
            ticket, sites, verbose=opts.verbose,
        )
        if site_entry is None:
            _emit(f"[{_now_local_hms()}] #{tid} skipped: site unresolvable")
            if on_failure is not None:
                on_failure(tid, "site unresolvable")
            continue

        try:
            if on_progress is not None:
                on_progress(tid, "triaging")
            report = pipeline.triage_one(
                ticket,
                site_entry,
                dd_client=dd_client,
                window_minutes=opts.window_minutes,
                levels=opts.levels,
                at=None,
                verbose=opts.verbose,
                show_spinner=False,
            )
        except (RuntimeError, ValueError) as e:
            _emit(f"[{_now_local_hms()}] #{tid} failed: {e} (will retry)")
            if on_failure is not None:
                on_failure(tid, str(e))
            continue

        try:
            md_path, _json_path = render.save_note(report, ticket.id)
        except OSError as e:
            _emit(f"[{_now_local_hms()}] #{tid} failed: could not write note: {e} (will retry)")
            if on_failure is not None:
                on_failure(tid, f"could not write note: {e}")
            continue
        _emit(f"[{_now_local_hms()}] #{tid} triaged → {md_path}")
        if on_complete is not None:
            on_complete(report)
        if opts.verbose:
            sources_str = ", ".join(report.sources)
            _emit(
                f"[{_now_local_hms()}] #{tid} confidence: {report.confidence}; "
                f"events: {report.log_event_count}; sources: {sources_str}"
            )
        if opts.print_notes:
            print(render.to_markdown(report).rstrip() + "\n---", flush=True)
        triaged_map[key] = ticket.updated_at.isoformat()

    return new_state


def run_watch(opts: WatcherOptions) -> None:
    """Main loop. Polls a view, triages new/updated tickets, sleeps, repeats.

    Exits cleanly on KeyboardInterrupt. On unrecoverable startup errors
    (cannot load site map, missing Zendesk env), raises RuntimeError so
    the CLI can print and exit.
    """
    sites = extract.load_site_map(Path("data/cnc-map.json"))
    state = load_state(opts.state_file)
    cutoff = (
        datetime.now(UTC) - timedelta(hours=opts.backfill_hours)
        if math.isfinite(opts.backfill_hours)
        else datetime.min.replace(tzinfo=UTC)
    )

    iteration = 0
    try:
        while True:
            iteration += 1
            _emit(f"[{_now_local_hms()}] iteration {iteration} start (view={opts.view_id})")
            with ZendeskClient.from_env() as zd:
                if opts.no_logs:
                    state = run_iteration(zd, sites, state, opts, cutoff, dd_client=None)
                else:
                    with DatadogClient.from_env() as dd:
                        state = run_iteration(zd, sites, state, opts, cutoff, dd_client=dd)
            save_state(opts.state_file, prune_state(state))
            _emit(f"[{_now_local_hms()}] iteration {iteration} done; sleeping {opts.interval}s")
            time.sleep(opts.interval)
    except KeyboardInterrupt:
        _emit(f"[{_now_local_hms()}] watcher stopped (Ctrl-C)")
        save_state(opts.state_file, prune_state(state))
