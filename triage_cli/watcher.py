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
import os
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from triage_cli.models import Ticket

logger = logging.getLogger(__name__)

STATE_VERSION = 1
DEFAULT_PRUNE_CAP = 1000


@dataclass(frozen=True)
class WatcherOptions:
    view_id: int
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
        stored_dt = stored_dt.replace(tzinfo=timezone.utc)
    return ticket.updated_at > stored_dt


def prune_state(state: State, max_entries: int = DEFAULT_PRUNE_CAP) -> State:
    """Keep at most max_entries triaged entries, dropping the oldest by timestamp."""
    triaged = state.get("triaged") or {}
    if len(triaged) <= max_entries:
        return {"version": STATE_VERSION, "triaged": dict(triaged)}
    items = sorted(triaged.items(), key=lambda kv: kv[1], reverse=True)
    kept = dict(items[:max_entries])
    return {"version": STATE_VERSION, "triaged": kept}
