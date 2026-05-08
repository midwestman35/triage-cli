"""Investigation session: state and orchestration for guided triage.

An `InvestigationSession` accumulates evidence sources and the timeline events
they emit, then can be assessed by the LLM into a `TriageReport`. The session
is mutable so callers (the `investigate` command, future TUI, future loops)
can add evidence incrementally and re-assess. Each `run_assessment` call is
stateless against the LLM — it sends the current snapshot and gets a fresh
output.

This module owns the prompt format for the new investigation flow. The legacy
one-shot `triage` command keeps using `TriageBundle` and the older prompt; the
two paths intentionally don't share prompt code yet.
"""
from __future__ import annotations

import asyncio
from datetime import UTC, datetime

from pydantic import BaseModel, Field

from triage_cli.evidence import EvidenceSource
from triage_cli.models import (
    LLMTriageOutput,
    Ticket,
    TimeWindow,
    TriageReport,
    fmt_ts,
    indent_continuations,
)
from triage_cli.timeline import TimelineEvent, merge

DEFAULT_TIMELINE_CAP = 500


class InvestigationSession(BaseModel):
    """Mutable working state for one guided triage of one ticket."""

    ticket: Ticket
    sources: list[EvidenceSource] = Field(default_factory=list)
    timeline: list[TimelineEvent] = Field(default_factory=list)
    report: TriageReport | None = None


def add_source(
    session: InvestigationSession,
    source: EvidenceSource,
    events: list[TimelineEvent],
) -> None:
    """Register a new evidence source with its produced events.

    Re-merges the timeline so order is maintained. Cheap until the timeline
    grows past a few thousand events; revisit if that ever happens.
    """
    session.sources.append(source)
    session.timeline = merge(session.timeline, events)


def to_assessment_prompt(
    session: InvestigationSession, *, timeline_cap: int = DEFAULT_TIMELINE_CAP,
) -> str:
    """Render the full LLM user prompt for an investigation assessment.

    Sections:
        # Ticket          — header (subject, requester, tags); no comment bodies
        # Description     — verbatim ticket description
        # Evidence sources — manifest of EvidenceSources
        # Timeline         — chronological merged events, capped at `timeline_cap`
    """
    t = session.ticket
    tags_str = ", ".join(t.tags) if t.tags else "(none)"
    org_str = t.requester_org if t.requester_org else "(unset)"

    lines: list[str] = []
    lines.append(f"# Ticket #{t.id}")
    lines.append(f"Subject: {t.subject}")
    lines.append(f"Created: {fmt_ts(t.created_at)}")
    lines.append(f"Requester org: {org_str}")
    lines.append(f"Tags: {tags_str}")
    lines.append("")
    lines.append("## Description")
    lines.append(indent_continuations(t.description) if t.description else "(empty)")
    lines.append("")

    lines.append("# Evidence sources")
    if session.sources:
        for s in session.sources:
            marker = "" if s.parsed else " [metadata only]"
            count = f" ({s.event_count} events)" if s.event_count else ""
            note = f" — {s.notes}" if s.notes else ""
            lines.append(f"- {s.kind.value}: {s.label}{count}{marker}{note}")
    else:
        lines.append("- (none beyond the ticket itself)")
    lines.append("")

    lines.append("# Timeline (chronological; untimed events at end)")
    if not session.timeline:
        lines.append("(no events)")
    else:
        truncated = len(session.timeline) > timeline_cap
        for e in session.timeline[:timeline_cap]:
            lines.append(_render_event(e))
        if truncated:
            dropped = len(session.timeline) - timeline_cap
            lines.append(f"... ({dropped} additional event(s) truncated)")
    return "\n".join(lines)


def _render_event(e: TimelineEvent) -> str:
    ts = fmt_ts(e.timestamp) if e.timestamp is not None else "(no timestamp)"
    level = f" [{e.level}]" if e.level else ""
    body = indent_continuations(e.message)
    src = f"{e.source}/{e.kind}"
    return f"- {ts} {src}{level}: {body}"


def _derive_window(timeline: list[TimelineEvent], fallback: datetime) -> TimeWindow:
    """Min/max of timestamped events, or a single-point window at the fallback."""
    timed = [e.timestamp for e in timeline if e.timestamp is not None]
    if not timed:
        return TimeWindow(start=fallback, end=fallback)
    return TimeWindow(start=min(timed), end=max(timed))


def _source_strings(sources: list[EvidenceSource]) -> list[str]:
    """Serialize the source manifest to the `TriageReport.sources` shape."""
    out: list[str] = []
    for s in sources:
        base = f"{s.kind.value}:{s.label}"
        out.append(f"{base}({s.event_count})" if s.event_count else base)
    return out


def build_report(session: InvestigationSession, llm_out: LLMTriageOutput) -> TriageReport:
    """Wrap an LLMTriageOutput in a TriageReport using session-derived metadata."""
    timed_count = sum(1 for e in session.timeline if e.timestamp is not None)
    window = _derive_window(session.timeline, fallback=session.ticket.created_at)
    return TriageReport(
        **llm_out.model_dump(),
        ticket_id=session.ticket.id,
        site_name=None,
        window=window,
        sources=["zendesk"] + _source_strings(session.sources),
        log_event_count=timed_count,
        generated_at=datetime.now(UTC),
    )


def run_assessment(
    session: InvestigationSession, *, verbose: bool = False,
) -> TriageReport:
    """Send the current session to the LLM and produce a TriageReport.

    Updates `session.report` in place and returns the same object.
    Raises RuntimeError on LLM transport failure or invalid JSON after retry.
    """
    # Imported lazily so the test suite can monkeypatch llm.assess without
    # forcing `claude_agent_sdk` to be importable at module load.
    from triage_cli.llm import assess as _llm_assess

    llm_out = asyncio.run(_llm_assess(session, verbose=verbose))
    report = build_report(session, llm_out)
    session.report = report
    return report
