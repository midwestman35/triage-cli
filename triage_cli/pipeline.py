"""End-to-end triage pipeline for a fetched ticket and a resolved site.

Owns the LLM and Datadog calls; does not handle ticket fetch, site
resolution, output rendering, or persistence. Two callers today:
`cli.triage` (with interactive site prompt) and `watcher.run_iteration`
(skips on no-match).
"""
from __future__ import annotations

import asyncio
import contextlib
import logging
import sys
from collections.abc import Iterator
from datetime import UTC, datetime

from unicode_animations import live_spinner as _live_spinner

from triage_cli import extract
from triage_cli.datadog import DatadogClient
from triage_cli.llm import extract_anchor as _llm_extract_anchor
from triage_cli.llm import triage as _llm_triage
from triage_cli.models import SiteEntry, Ticket, TimeWindow, TriageBundle, TriageReport

logger = logging.getLogger(__name__)


@contextlib.contextmanager
def spinner(text: str, *, show: bool) -> Iterator[None]:
    """Show an 'orbit' loading spinner during a slow op when stderr is a TTY."""
    if show and sys.stderr.isatty():
        with _live_spinner("orbit", text=text, stream=sys.stderr):
            yield
    else:
        yield


def _vecho(verbose: bool, msg: str) -> None:
    """Echo to stderr only when verbose is set."""
    if verbose:
        print(msg, file=sys.stderr, flush=True)


def triage_one(
    ticket: Ticket,
    site_entry: SiteEntry,
    *,
    dd_client: DatadogClient | None,
    window_minutes: int,
    levels: list[str],
    at: datetime | None,
    verbose: bool,
    show_spinner: bool,
) -> TriageReport:
    """Run the triage pipeline for a fetched ticket and resolved site.

    Returns a `TriageReport` (LLM output + pipeline-derived metadata).
    Raises RuntimeError on Datadog or Claude failure.
    Raises ValueError on validation failure (e.g. invalid window).
    """
    # Anchor: best-effort LLM extraction unless --at was supplied.
    extracted_dt: datetime | None = None
    if dd_client is not None and at is None:
        try:
            with spinner("Asking Claude to extract incident timestamp", show=show_spinner):
                extracted_dt = asyncio.run(_llm_extract_anchor(ticket))
        except Exception as e:
            _vecho(verbose, f"Anchor extraction via Claude failed: {e}; falling back to created_at")

    anchor_dt, anchor_source = extract.resolve_anchor(
        ticket, at_flag=at, extracted=extracted_dt,
    )
    _vecho(verbose, f"Anchor: {anchor_dt.isoformat()} from {anchor_source.value}")

    start, end = extract.build_window(anchor_dt, window_minutes)
    log_lines: list = []
    log_truncated = False
    if dd_client is None:
        _vecho(verbose, "Skipping Datadog (--no-logs)")
    else:
        with spinner(f"Querying Datadog for {site_entry.site_name}", show=show_spinner):
            log_lines, log_truncated = dd_client.get_logs(
                site_entry.site_name, levels, start, end,
            )
        _vecho(verbose, f"Pulled {len(log_lines)} log lines (truncated={log_truncated})")

    bundle = TriageBundle(
        ticket=ticket,
        site_entry=site_entry,
        log_lines=log_lines,
        log_truncated=log_truncated,
        anchor=anchor_dt,
        anchor_source=anchor_source,
        window_start=start,
        window_end=end,
    )

    with spinner("Generating triage note", show=show_spinner):
        llm_out = asyncio.run(_llm_triage(bundle, verbose=verbose))

    sources = ["zendesk"] + (["datadog"] if dd_client is not None else [])

    return TriageReport(
        **llm_out.model_dump(),
        ticket_id=ticket.id,
        site_name=site_entry.site_name,
        window=TimeWindow(start=start, end=end),
        sources=sources,
        log_event_count=len(log_lines),
        generated_at=datetime.now(UTC),
    )
