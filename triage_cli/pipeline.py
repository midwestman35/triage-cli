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
from collections.abc import Callable, Iterator
from datetime import UTC, datetime
from pathlib import Path
from typing import TYPE_CHECKING, Any, Protocol, runtime_checkable

if TYPE_CHECKING:
    from triage_cli.models import InvestigationSession

from unicode_animations import live_spinner as _live_spinner

from triage_cli import extract
from triage_cli.datadog import DatadogClient
from triage_cli.llm import extract_anchor as _llm_extract_anchor
from triage_cli.llm import extract_site as _llm_extract_site
from triage_cli.llm import triage as _llm_triage
from triage_cli.models import SiteEntry, Ticket, TimeWindow, TriageBundle, TriageReport

logger = logging.getLogger(__name__)


@runtime_checkable
class Reporter(Protocol):
    """Decouples progress output from pipeline logic.

    StderrReporter (default), TUIReporter (--tui), SilentReporter (tests).
    """

    def phase_started(self, phase: str, detail: str = "") -> None: ...
    def phase_done(self, phase: str, detail: str = "") -> None: ...
    def phase_failed(self, phase: str, err: Exception) -> None: ...
    def evidence_added(self, item: Any) -> None: ...
    def done(self, report: TriageReport) -> None: ...


class StderrReporter:
    """Write pipeline progress to stderr. phase_started is gated by verbose."""

    def __init__(self, verbose: bool = True) -> None:
        self._verbose = verbose

    def phase_started(self, phase: str, detail: str = "") -> None:
        if self._verbose:
            msg = f"→ {phase}" + (f": {detail}" if detail else "")
            print(msg, file=sys.stderr, flush=True)

    def phase_done(self, phase: str, detail: str = "") -> None:
        msg = f"✓ {phase}" + (f": {detail}" if detail else "")
        print(msg, file=sys.stderr, flush=True)

    def phase_failed(self, phase: str, err: Exception) -> None:
        print(f"✗ {phase}: {err}", file=sys.stderr, flush=True)

    def evidence_added(self, item: Any) -> None:
        pass

    def done(self, report: TriageReport) -> None:
        pass


class SilentReporter:
    """No-op reporter for tests and CI."""

    def phase_started(self, phase: str, detail: str = "") -> None:
        pass

    def phase_done(self, phase: str, detail: str = "") -> None:
        pass

    def phase_failed(self, phase: str, err: Exception) -> None:
        pass

    def evidence_added(self, item: Any) -> None:
        pass

    def done(self, report: TriageReport) -> None:
        pass


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


def resolve_site(
    ticket: Ticket,
    sites: list[SiteEntry],
    *,
    cnc_override: str | None = None,
    site_override: str | None = None,
    verbose: bool = False,
    show_spinner: bool = False,
    redact_enabled: bool = True,
) -> tuple[SiteEntry | None, str]:
    """Resolve which SiteEntry a ticket is about, with LLM fallback on no_match.

    Runs extract.lookup_site first (fast, pure). If that returns no_match,
    asks Claude to identify the site from the ticket text against the known list.
    Returns (entry, strategy); strategy is 'llm_extraction' when the LLM wins.
    Returns (None, 'no_match') when both strategies fail.
    """
    site_entry, strategy = extract.lookup_site(
        ticket, sites, cnc_override=cnc_override, site_override=site_override,
    )
    if site_entry is not None:
        return site_entry, strategy

    _vecho(verbose, "Site lookup: no_match — asking Claude to identify site")
    try:
        with spinner("Asking Claude to identify site", show=show_spinner):
            llm_name = asyncio.run(
                _llm_extract_site(ticket, sites, redact_enabled=redact_enabled, verbose=verbose)
            )
    except Exception as e:
        _vecho(verbose, f"LLM site extraction failed: {e}")
        return None, "no_match"

    if llm_name is None:
        _vecho(verbose, "LLM could not identify site")
        return None, "no_match"

    site_entry, _ = extract.lookup_site(ticket, sites, site_override=llm_name)
    if site_entry is not None:
        _vecho(verbose, f"LLM identified site: {llm_name}")
        return site_entry, "llm_extraction"

    return None, "no_match"


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
    downloaded_attachments: list | None = None,
    local_files: list | None = None,
    pasted_logs: list | None = None,
    on_phase: Callable[[str, int], None] | None = None,
    redact_enabled: bool = True,
) -> TriageReport:
    """Run the triage pipeline for a fetched ticket and resolved site.

    Returns a `TriageReport` (LLM output + pipeline-derived metadata).
    Raises RuntimeError on Datadog or Claude failure.
    Raises ValueError on validation failure (e.g. invalid window).
    Optional evidence kwargs default to empty lists.
    """
    # Anchor: best-effort LLM extraction unless --at was supplied.
    extracted_dt: datetime | None = None
    if dd_client is not None and at is None:
        try:
            if on_phase is not None:
                on_phase("Extracting anchor timestamp", 2)
            with spinner("Asking Claude to extract incident timestamp", show=show_spinner):
                extracted_dt = asyncio.run(
                    _llm_extract_anchor(ticket, redact_enabled=redact_enabled, verbose=verbose)
                )
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
        if on_phase is not None:
            on_phase("Querying Datadog", 3)
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
        downloaded_attachments=downloaded_attachments or [],
        local_files=local_files or [],
        pasted_logs=pasted_logs or [],
    )

    if on_phase is not None:
        on_phase("Asking Claude", 4)
    with spinner("Generating triage note", show=show_spinner):
        llm_out, redaction_counts = asyncio.run(
            _llm_triage(bundle, verbose=verbose, redact_enabled=redact_enabled)
        )

    sources = ["zendesk"] + (["datadog"] if dd_client is not None else [])

    return TriageReport(
        **llm_out.model_dump(),
        ticket_id=ticket.id,
        site_name=site_entry.site_name,
        window=TimeWindow(start=start, end=end),
        sources=sources,
        log_event_count=len(log_lines),
        generated_at=datetime.now(UTC),
        redaction_summary=redaction_counts,
    )


def _stub_assess(ticket: Ticket, session: InvestigationSession) -> TriageReport:
    """Deterministic assessment without an LLM call. Used with --no-llm."""
    count = len(session.timeline)
    if count > 10:
        confidence = "high"
    elif count < 3:
        confidence = "low"
    else:
        confidence = "medium"
    return TriageReport(
        ticket_id=ticket.id,
        finding=f"[stub] No LLM call. {count} timeline event(s).",
        confidence=confidence,
        evidence=[],
        suggested_note="[stub] Rerun without --no-llm for a real assessment.",
        next_checks=["Rerun without --no-llm"],
        unknowns=["LLM assessment not performed"],
        sources=["stub"],
        generated_at=datetime.now(UTC),
    )


async def investigate_one(
    ticket: Ticket,
    *,
    session: InvestigationSession,
    dd_client: DatadogClient | None = None,
    reporter: Reporter,
    interactive: bool = True,
    workspace: Path | None = None,
    cnc_override: str | None = None,
    site_override: str | None = None,
    anchor_override: datetime | None = None,
    window_minutes: int = 30,
    levels: list[str] | None = None,
    verbose: bool = False,
    redact_enabled: bool = True,
    no_llm: bool = False,
) -> TriageReport:
    """Shared investigation core used by investigate, triage, and watch.

    Enriches session with customer history + memory context, runs optional
    site/Datadog enrichment, calls the configured LLM provider (or stub),
    writes artifacts, and appends to the memory layer.
    """
    from pathlib import Path as _Path

    from triage_cli import memory as mem
    from triage_cli import render
    from triage_cli.models import (
        AnchorSource,
        CustomerHistoryEvidence,
        MemoryContext,
        TriageBundle,
    )
    from triage_cli.zendesk import ZendeskClient

    effective_levels = levels or ["error", "warn", "info"]

    # Phase: customer_history
    reporter.phase_started("customer_history", "fetching requester history")
    try:
        zd_client = ZendeskClient.from_env()
        history_tickets = zd_client.fetch_customer_history(
            ticket.requester_email or "", limit=10,
        )
        if history_tickets:
            session.evidence.customer_history = CustomerHistoryEvidence(
                requester_email=ticket.requester_email or "",
                tickets=history_tickets,
                limit=10,
            )
        reporter.phase_done(
            "customer_history",
            f"{len(history_tickets)} prior ticket(s) found",
        )
    except Exception as e:
        reporter.phase_failed("customer_history", e)

    # Phase: memory_lookup
    reporter.phase_started("memory_lookup", "querying prior investigations")
    prior = mem.retrieve_similar(
        ticket.subject,
        (ticket.description or "")[:500],
        limit=3,
    )
    duplicate = mem.find_duplicate(str(ticket.id))
    if duplicate:
        print(
            f"⚠ ZD-{ticket.id} was previously investigated",
            file=sys.stderr,
            flush=True,
        )
    session.memory_context = MemoryContext(
        entries=prior,
        query_tokens=ticket.subject.lower().split(),
    )
    reporter.phase_done("memory_lookup", f"{len(prior)} prior investigation(s) found")

    # Phase: evidence_intake (interactive only — CLI handles this before calling us)
    if interactive:
        reporter.phase_started("evidence_intake")
        reporter.phase_done("evidence_intake")

    # Phase: build_timeline
    reporter.phase_started("build_timeline")
    reporter.phase_done("build_timeline", f"{len(session.timeline)} event(s)")

    # Phase: enrichment (optional Datadog)
    reporter.phase_started("enrichment")
    site_entry = None
    log_lines: list = []
    log_truncated = False
    if dd_client is not None:
        try:
            sites_path = _Path("data/cnc-map.json")
            if sites_path.exists():
                import json as _json

                from triage_cli.models import SiteEntry
                raw_sites = _json.loads(sites_path.read_text())
                sites = [SiteEntry(**s) for s in raw_sites]
                site_entry, _ = resolve_site(
                    ticket, sites,
                    cnc_override=cnc_override,
                    site_override=site_override,
                    verbose=verbose,
                )
                if site_entry:
                    extracted_dt: datetime | None = None
                    if anchor_override is None:
                        with contextlib.suppress(Exception):
                            extracted_dt = await _llm_extract_anchor(ticket)
                    anchor_dt, _ = extract.resolve_anchor(
                        ticket, at_flag=anchor_override, extracted=extracted_dt,
                    )
                    start, end = extract.build_window(anchor_dt, window_minutes)
                    log_lines, log_truncated = dd_client.get_logs(
                        site_entry.site_name, effective_levels, start, end,
                    )
            reporter.phase_done("enrichment", f"{len(log_lines)} log line(s)")
        except Exception as e:
            reporter.phase_failed("enrichment", e)
    else:
        reporter.phase_done("enrichment", "skipped (no Datadog client)")

    # Phase: llm_call
    reporter.phase_started("llm_call", "generating assessment")
    if no_llm:
        report = _stub_assess(ticket, session)
        reporter.phase_done("llm_call", "stub (--no-llm)")
    else:
        from triage_cli import llm as _llm_mod
        bundle = TriageBundle(
            ticket=ticket,
            site_entry=site_entry,
            log_lines=log_lines,
            log_truncated=log_truncated,
            anchor=anchor_override or ticket.created_at,
            anchor_source=AnchorSource.FLAG if anchor_override else AnchorSource.CREATED_AT,
            window_start=None,
            window_end=None,
            downloaded_attachments=list(session.evidence.attachments),
            local_files=list(session.evidence.local_files),
            pasted_logs=list(session.evidence.pasted_logs),
            customer_history=session.evidence.customer_history,
            memory_context=session.memory_context,
        )
        llm_out = await _llm_mod.triage(bundle, verbose=verbose)
        report = TriageReport(
            **llm_out.model_dump(),
            ticket_id=ticket.id,
            site_name=site_entry.site_name if site_entry else None,
            sources=["zendesk"] + (["datadog"] if dd_client and log_lines else []),
            log_event_count=len(log_lines),
            generated_at=datetime.now(UTC),
        )
        reporter.phase_done("llm_call", f"confidence={report.confidence}")

    # Phase: save
    reporter.phase_started("save")
    notes_dir = _Path("triage-notes")
    notes_dir.mkdir(exist_ok=True)
    render.save_note(report, ticket.id, notes_dir)
    mem.append_investigation(
        ticket_id=str(ticket.id),
        customer=ticket.requester_email or "unknown",
        subject=ticket.subject,
        symptom=(ticket.description or "")[:500],
        assessment=report.finding,
    )
    reporter.phase_done("save")
    reporter.done(report)
    return report
