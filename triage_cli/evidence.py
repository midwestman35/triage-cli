"""Evidence sources: ingestion helpers that produce normalized timeline events.

This module is the seam where heterogeneous inputs (local files, pasted text,
Zendesk attachments, future Datadog queries) become a uniform pair of
`(EvidenceSource, list[TimelineEvent])`. The `InvestigationSession` accumulates
sources and folds the per-source events into one merged timeline.

Zendesk attachment ingestion is metadata-only this pass: we record filename,
content type, and size from the comment payload, but we do not download bytes
or parse content. Marking that as a follow-up is intentional — call recordings
and CAD exports are a different privacy class than the comment text we already
send to the LLM.
"""
from __future__ import annotations

from collections.abc import Iterable
from datetime import datetime
from enum import StrEnum
from pathlib import Path
from typing import Any

from pydantic import BaseModel, Field

from triage_cli.models import Comment, Ticket
from triage_cli.timeline import TimelineEvent, parse_lines


class EvidenceKind(StrEnum):
    """The discriminator for an `EvidenceSource`."""

    ZENDESK_TICKET = "zendesk_ticket"
    ZENDESK_COMMENT = "zendesk_comment"
    ZENDESK_ATTACHMENT = "zendesk_attachment"
    LOCAL_FILE = "local_file"
    LOCAL_DIRECTORY = "local_directory"
    PASTED_TEXT = "pasted_text"
    DATADOG_QUERY = "datadog_query"


class EvidenceSource(BaseModel):
    """One piece of evidence registered with an `InvestigationSession`.

    `parsed` is True when the source produced TimelineEvents; False when it is
    metadata-only (e.g. a Zendesk attachment whose bytes weren't fetched).
    `event_count` is the number of TimelineEvents produced; `notes` carries
    parser-side context like the unparsed-line count or download status.
    """

    kind: EvidenceKind
    label: str
    source_ref: str
    parsed: bool = True
    event_count: int = 0
    truncated: bool = False
    notes: str | None = None
    extra: dict[str, Any] = Field(default_factory=dict)


def from_ticket(ticket: Ticket) -> tuple[EvidenceSource, list[TimelineEvent]]:
    """Synthesize a TimelineEvent for the ticket creation itself."""
    src = EvidenceSource(
        kind=EvidenceKind.ZENDESK_TICKET,
        label=f"ZD-{ticket.id}",
        source_ref=f"zendesk:ticket:{ticket.id}",
        parsed=True,
        event_count=1,
    )
    event = TimelineEvent(
        timestamp=ticket.created_at,
        source=src.label,
        kind="ticket_created",
        message=f"Ticket created: {ticket.subject}",
        attributes={"requester_org": ticket.requester_org or "", "tags": list(ticket.tags)},
    )
    return src, [event]


def from_comments(ticket: Ticket) -> tuple[EvidenceSource, list[TimelineEvent]]:
    """One EvidenceSource that wraps the full comment thread as TimelineEvents."""
    events: list[TimelineEvent] = []
    label = f"ZD-{ticket.id} comments"
    for c in ticket.comments:
        events.append(_comment_event(c, label))
    src = EvidenceSource(
        kind=EvidenceKind.ZENDESK_COMMENT,
        label=label,
        source_ref=f"zendesk:ticket:{ticket.id}:comments",
        parsed=True,
        event_count=len(events),
    )
    return src, events


def _comment_event(c: Comment, source_label: str) -> TimelineEvent:
    visibility = "public" if c.is_public else "internal"
    return TimelineEvent(
        timestamp=c.created_at,
        source=source_label,
        kind="zendesk_comment",
        message=c.body,
        attributes={"author": c.author, "visibility": visibility},
    )


def from_local_file(path: Path) -> tuple[EvidenceSource, list[TimelineEvent]]:
    """Read a text log file and parse its lines into TimelineEvents.

    Raises FileNotFoundError if the path doesn't exist.
    Raises UnicodeDecodeError on non-text content (caller decides how to surface).
    """
    text = path.read_text(encoding="utf-8", errors="replace")
    label = path.name
    events, unparsed = parse_lines(text, source=label)
    notes = f"{unparsed} unparsed line(s)" if unparsed else None
    src = EvidenceSource(
        kind=EvidenceKind.LOCAL_FILE,
        label=label,
        source_ref=str(path.resolve()),
        parsed=True,
        event_count=len(events),
        notes=notes,
        extra={"size_bytes": path.stat().st_size},
    )
    return src, events


def from_local_directory(
    path: Path, pattern: str = "*.log",
) -> tuple[EvidenceSource, list[TimelineEvent]]:
    """Glob a directory and parse every matching file into one merged source."""
    if not path.is_dir():
        raise NotADirectoryError(f"Not a directory: {path}")
    matched = sorted(path.glob(pattern))
    events: list[TimelineEvent] = []
    unparsed_total = 0
    for fp in matched:
        if not fp.is_file():
            continue
        try:
            text = fp.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        evs, unparsed = parse_lines(text, source=fp.name)
        events.extend(evs)
        unparsed_total += unparsed
    notes_parts = [f"{len(matched)} file(s) matched {pattern!r}"]
    if unparsed_total:
        notes_parts.append(f"{unparsed_total} unparsed line(s)")
    src = EvidenceSource(
        kind=EvidenceKind.LOCAL_DIRECTORY,
        label=path.name or str(path),
        source_ref=str(path.resolve()),
        parsed=True,
        event_count=len(events),
        notes="; ".join(notes_parts),
    )
    return src, events


def from_pasted_text(
    text: str, label: str = "pasted",
) -> tuple[EvidenceSource, list[TimelineEvent]]:
    """Parse user-pasted text as a log stream."""
    events, unparsed = parse_lines(text, source=label)
    notes = f"{unparsed} unparsed line(s)" if unparsed else None
    src = EvidenceSource(
        kind=EvidenceKind.PASTED_TEXT,
        label=label,
        source_ref=f"paste:{label}",
        parsed=True,
        event_count=len(events),
        notes=notes,
    )
    return src, events


def attachments_metadata(
    raw_attachments: Iterable[dict[str, Any]],
    *, ticket_id: int, comment_id: int | None = None,
) -> list[EvidenceSource]:
    """Build metadata-only EvidenceSources from Zendesk attachment payloads.

    Bytes are not downloaded in this pass. Each EvidenceSource is marked
    `parsed=False` with a note explaining the follow-up.
    """
    out: list[EvidenceSource] = []
    for att in raw_attachments:
        filename = str(att.get("file_name") or att.get("filename") or "(unnamed)")
        content_type = att.get("content_type")
        size = att.get("size")
        url = att.get("content_url") or att.get("url")
        ref_parts = [f"zendesk:ticket:{ticket_id}"]
        if comment_id is not None:
            ref_parts.append(f"comment:{comment_id}")
        ref_parts.append(f"file:{filename}")
        out.append(
            EvidenceSource(
                kind=EvidenceKind.ZENDESK_ATTACHMENT,
                label=filename,
                source_ref=":".join(ref_parts),
                parsed=False,
                event_count=0,
                notes="metadata only; binary download not yet implemented",
                extra={
                    "content_type": content_type,
                    "size_bytes": size,
                    "content_url": url,
                },
            )
        )
    return out


def summarize_sources(sources: list[EvidenceSource]) -> str:
    """One-line summary of the source manifest, for prompts and verbose echo."""
    if not sources:
        return "(no evidence sources)"
    parts: list[str] = []
    for s in sources:
        marker = "" if s.parsed else " [meta]"
        count = f" ({s.event_count} events)" if s.event_count else ""
        parts.append(f"{s.kind.value}:{s.label}{count}{marker}")
    return "; ".join(parts)


def first_seen(events: list[TimelineEvent]) -> datetime | None:
    """Earliest non-null timestamp in the stream, or None if all are untimed."""
    return min((e.timestamp for e in events if e.timestamp is not None), default=None)
