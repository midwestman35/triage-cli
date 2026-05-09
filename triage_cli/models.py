"""Pydantic data models for the triage-cli pipeline."""
from __future__ import annotations

from datetime import UTC, datetime
from enum import StrEnum
from pathlib import Path
from typing import Any, Literal

from pydantic import BaseModel, Field, field_validator


def fmt_ts(dt: datetime) -> str:
    """Render a datetime as ISO 8601 in UTC with Z suffix, no microseconds."""
    dt = dt.replace(tzinfo=UTC) if dt.tzinfo is None else dt.astimezone(UTC)
    return dt.replace(microsecond=0).isoformat().replace("+00:00", "Z")


def indent_continuations(s: str) -> str:
    """Indent continuation lines so wrapped bullets remain visually attached."""
    return s.replace("\n", "\n  ")


class AnchorSource(StrEnum):
    """Where the anchor timestamp on a TriageBundle came from."""

    FLAG = "flag"
    EXTRACTED = "extracted"
    CREATED_AT = "created_at"


class AttachmentEvidence(BaseModel):
    """Metadata for an attachment discovered on a Zendesk ticket."""

    filename: str
    content_type: str | None = None
    size_bytes: int | None = None
    source: Literal["zendesk_attachment"] = "zendesk_attachment"
    local_path: Path | None = None
    extracted_text: str | None = None
    content_url: str | None = None


class Comment(BaseModel):
    """A single Zendesk ticket comment, public or internal."""

    author: str
    body: str
    created_at: datetime
    is_public: bool
    attachments: list[AttachmentEvidence] = Field(default_factory=list)


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


class LocalFileEvidence(BaseModel):
    """Evidence read from a local path supplied during guided investigation."""

    path: Path
    size_bytes: int | None = None
    detected_type: Literal["text", "log", "json", "unknown"] | None = None
    extracted_text: str | None = None


class PastedEvidence(BaseModel):
    """User-pasted text evidence captured during guided investigation."""

    label: str
    text: str


class InvestigationEvidence(BaseModel):
    """All evidence gathered for an investigation session."""

    ticket_id: int
    comments: list[Comment] = Field(default_factory=list)
    attachments: list[AttachmentEvidence] = Field(default_factory=list)
    local_files: list[LocalFileEvidence] = Field(default_factory=list)
    pasted_logs: list[PastedEvidence] = Field(default_factory=list)
    optional_sources: list[str] = Field(default_factory=list)


class TimelineEvent(BaseModel):
    """A normalized event in the investigation timeline."""

    timestamp: datetime | None = None
    source: str
    kind: str
    message: str
    raw_ref: str | None = None

    @field_validator("timestamp")
    @classmethod
    def _timestamp_as_utc(cls, value: datetime | None) -> datetime | None:
        if value is None:
            return None
        return value.replace(tzinfo=UTC) if value.tzinfo is None else value.astimezone(UTC)


class Assessment(BaseModel):
    """Deterministic investigation assessment suitable for a Zendesk handoff draft."""

    summary: str
    likely_root_cause: str
    confidence: Confidence
    correlation: list[str] = Field(default_factory=list)
    unknowns: list[str] = Field(default_factory=list)
    next_steps: list[str] = Field(default_factory=list)
    suggested_internal_note: str


class InvestigationSession(BaseModel):
    """State container for guided investigation before and after assessment."""

    ticket: Ticket
    evidence: InvestigationEvidence
    timeline: list[TimelineEvent] = Field(default_factory=list)
    assessment: Assessment | None = None
    report: TriageReport | None = None


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


Confidence = Literal["low", "medium", "high"]


class TimeWindow(BaseModel):
    """A timezone-aware UTC window. Both endpoints inclusive."""

    start: datetime
    end: datetime

    @field_validator("start", "end")
    @classmethod
    def _as_utc(cls, value: datetime) -> datetime:
        return value.replace(tzinfo=UTC) if value.tzinfo is None else value.astimezone(UTC)


class EvidenceItem(BaseModel):
    """A single piece of evidence cited by the LLM in support of its finding.

    `timestamp` and `service` are None when the evidence comes from the ticket
    text rather than a Datadog log line.
    """

    timestamp: datetime | None = None
    service: str | None = None
    message: str


class LLMTriageOutput(BaseModel):
    """The fields the LLM emits as JSON. Subset of `TriageReport`."""

    finding: str
    confidence: Confidence
    evidence: list[EvidenceItem]
    suggested_note: str
    next_checks: list[str] = Field(default_factory=list)
    unknowns: list[str] = Field(default_factory=list)


class TriageReport(LLMTriageOutput):
    """Full triage report: LLM output + pipeline-derived metadata."""

    ticket_id: int
    site_name: str
    window: TimeWindow
    sources: list[str]
    log_event_count: int
    generated_at: datetime

    @field_validator("generated_at")
    @classmethod
    def _generated_at_as_utc(cls, value: datetime) -> datetime:
        return value.replace(tzinfo=UTC) if value.tzinfo is None else value.astimezone(UTC)
