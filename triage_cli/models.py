"""Pydantic data models for the triage-cli pipeline."""
from __future__ import annotations

from datetime import datetime, timezone
from enum import Enum
from typing import Any

from pydantic import BaseModel, Field


def fmt_ts(dt: datetime) -> str:
    """Render a datetime as ISO 8601 in UTC with Z suffix, no microseconds."""
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    else:
        dt = dt.astimezone(timezone.utc)
    return dt.replace(microsecond=0).isoformat().replace("+00:00", "Z")


def indent_continuations(s: str) -> str:
    """Indent every line after the first by two spaces so multi-line content stays visually attached to its bullet/section."""
    return s.replace("\n", "\n  ")


class AnchorSource(str, Enum):
    """Where the anchor timestamp on a TriageBundle came from."""

    FLAG = "flag"
    EXTRACTED = "extracted"
    CREATED_AT = "created_at"


class Comment(BaseModel):
    """A single Zendesk ticket comment, public or internal."""

    author: str
    body: str
    created_at: datetime
    is_public: bool


class Ticket(BaseModel):
    """A Zendesk ticket with its full chronological comment thread."""

    id: int
    subject: str
    description: str
    requester_org: str | None = None
    tags: list[str] = Field(default_factory=list)
    created_at: datetime
    updated_at: datetime
    comments: list[Comment] = Field(default_factory=list)


class LogLine(BaseModel):
    """A single Datadog log entry within the triage window."""

    timestamp: datetime
    level: str
    message: str
    attributes: dict[str, Any] = Field(default_factory=dict)


class SiteEntry(BaseModel):
    """One entry in cnc-map.json mapping a customer to a Datadog site_name and CNC UUID."""

    friendly_name: str
    site_name: str
    cnc: str


class TriageBundle(BaseModel):
    """Inputs to the LLM triage call: ticket, customer context, and log window."""

    ticket: Ticket
    site_entry: SiteEntry
    log_lines: list[LogLine] = Field(default_factory=list)
    log_truncated: bool = False
    anchor: datetime
    anchor_source: AnchorSource
    window_start: datetime
    window_end: datetime

    def as_user_message(self) -> str:
        t = self.ticket
        s = self.site_entry

        tags_str = ", ".join(t.tags) if t.tags else "(none)"
        org_str = t.requester_org if t.requester_org else "(unset)"

        lines: list[str] = []
        lines.append("# Customer")
        lines.append(f"- Friendly name: {s.friendly_name}")
        lines.append(f"- Site: {s.site_name}")
        lines.append(f"- CNC: {s.cnc}")
        lines.append("")
        lines.append(f"# Ticket #{t.id}")
        lines.append(f"Subject: {t.subject}")
        lines.append(f"Created: {fmt_ts(t.created_at)}")
        lines.append(f"Requester org: {org_str}")
        lines.append(f"Tags: {tags_str}")
        lines.append("")
        lines.append("## Description")
        lines.append(indent_continuations(t.description))
        lines.append("")
        lines.append('## Comments (chronological; "[internal]" prefix for non-public)')
        if t.comments:
            for c in t.comments:
                prefix = "" if c.is_public else "[internal] "
                body = indent_continuations(c.body)
                lines.append(f"- {prefix}{fmt_ts(c.created_at)} — {c.author}: {body}")
        else:
            lines.append("(no comments)")
        lines.append("")

        n = len(self.log_lines)
        truncated_str = ", truncated" if self.log_truncated else ""
        header = (
            f"# Logs (anchor: {fmt_ts(self.anchor)} from {self.anchor_source.value}; "
            f"window: {fmt_ts(self.window_start)} to {fmt_ts(self.window_end)}; "
            f"{n} lines{truncated_str})"
        )
        lines.append(header)
        if self.log_lines:
            for log in self.log_lines:
                msg = indent_continuations(log.message)
                lines.append(f"- {fmt_ts(log.timestamp)} [{log.level}] {msg}")
        else:
            lines.append("(no logs in window)")

        return "\n".join(lines)


class TriageNote(BaseModel):
    """Raw markdown response from the triage LLM call; no schema enforcement."""

    markdown: str
