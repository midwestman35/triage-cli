"""Guided investigation session and evidence helpers."""
from __future__ import annotations

import json
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from triage_cli.models import (
    Assessment,
    AttachmentEvidence,
    Comment,
    EvidenceItem,
    InvestigationEvidence,
    InvestigationSession,
    LocalFileEvidence,
    PastedEvidence,
    Ticket,
    TimelineEvent,
    TimeWindow,
    TriageReport,
)

EVIDENCE_EXCERPT_LIMIT = 360


def create_session(ticket: Ticket) -> InvestigationSession:
    """Create a guided investigation session from a fetched Zendesk ticket."""
    comments = [comment for comment in ticket.comments if isinstance(comment, Comment)]
    evidence = InvestigationEvidence(ticket_id=ticket.id, comments=comments)
    timeline = [
        TimelineEvent(
            timestamp=ticket.created_at,
            source="zendesk",
            kind="ticket_created",
            message=f"Ticket created: {ticket.subject}",
            raw_ref=f"ticket:{ticket.id}",
        ),
    ]

    for index, comment in enumerate(ticket.comments):
        timeline.append(_comment_event(comment, index))
        for attachment in _attachments_from_comment(comment):
            evidence.attachments.append(attachment.evidence)
            timeline.append(attachment.event)

    _sort_timeline(timeline)
    return InvestigationSession(ticket=ticket, evidence=evidence, timeline=timeline)


def add_local_file(session: InvestigationSession, path: str | Path) -> LocalFileEvidence:
    """Add local filesystem evidence, reading text-ish files when possible."""
    local_path = Path(path)
    stat = local_path.stat()
    detected_type = _detect_file_type(local_path)
    extracted_text = _read_text_if_supported(local_path, detected_type)
    evidence = LocalFileEvidence(
        path=local_path,
        size_bytes=stat.st_size,
        detected_type=detected_type,
        extracted_text=extracted_text,
    )
    session.evidence.local_files.append(evidence)
    session.timeline.append(
        TimelineEvent(
            source="local_files",
            kind="local_file",
            message=f"Local file added: {local_path}",
            raw_ref=str(local_path),
        ),
    )
    return evidence


def add_pasted_evidence(
    session: InvestigationSession,
    label: str,
    text: str,
) -> PastedEvidence:
    """Add user-pasted text as investigation evidence."""
    evidence = PastedEvidence(label=label, text=text)
    session.evidence.pasted_logs.append(evidence)
    session.timeline.append(
        TimelineEvent(
            source="pasted_logs",
            kind="pasted_log",
            message=f"Pasted evidence added: {label}",
            raw_ref=label,
        ),
    )
    return evidence


def build_timeline(session: InvestigationSession) -> list[TimelineEvent]:
    """Rebuild and return the current simple evidence timeline."""
    timeline = [
        TimelineEvent(
            timestamp=session.ticket.created_at,
            source="zendesk",
            kind="ticket_created",
            message=f"Ticket created: {session.ticket.subject}",
            raw_ref=f"ticket:{session.ticket.id}",
        ),
    ]
    for index, comment in enumerate(session.evidence.comments):
        timeline.append(_comment_event(comment, index))
    attachment_events = (
        [
            _attachment_event(evidence, index)
            for index, evidence in enumerate(session.evidence.attachments)
        ]
        if session.evidence.attachments
        else [
            attachment.event
            for comment in session.evidence.comments
            for attachment in _attachments_from_comment(comment)
        ]
    )
    timeline.extend(attachment_events)
    timeline.extend(
        TimelineEvent(
            source="local_files",
            kind="local_file",
            message=f"Local file added: {evidence.path}",
            raw_ref=str(evidence.path),
        )
        for evidence in session.evidence.local_files
    )
    timeline.extend(
        TimelineEvent(
            source="pasted_logs",
            kind="pasted_log",
            message=f"Pasted evidence added: {evidence.label}",
            raw_ref=evidence.label,
        )
        for evidence in session.evidence.pasted_logs
    )
    _sort_timeline(timeline)
    session.timeline = timeline
    return timeline


def assess_session(session: InvestigationSession) -> Assessment:
    """Produce a deterministic assessment without LLM, Datadog, or site lookup."""
    sources = _sources_for(session)
    has_extra_evidence = bool(session.evidence.local_files or session.evidence.pasted_logs)
    confidence = "medium" if has_extra_evidence else "low"
    summary = (
        f"Reviewed Zendesk ticket #{session.ticket.id} with "
        f"{len(session.evidence.comments)} comment(s) and evidence from {', '.join(sources)}."
    )
    likely_root_cause = (
        "Insufficient evidence for a specific root cause; ticket evidence should be correlated "
        "with local logs or attachment contents."
    )
    if has_extra_evidence:
        likely_root_cause = (
            "Available ticket and supplemental evidence indicate the reported symptom is present, "
            "but the root cause is not yet isolated."
        )

    correlation = [
        f"Ticket subject reports: {session.ticket.subject}",
        f"Ticket description captured: {_first_line(session.ticket.description)}",
    ]
    if session.evidence.comments:
        correlation.append(f"{len(session.evidence.comments)} Zendesk comment(s) reviewed.")
    if session.evidence.attachments:
        correlation.append(f"{len(session.evidence.attachments)} attachment(s) discovered.")
    if session.evidence.local_files:
        correlation.append(f"{len(session.evidence.local_files)} local file(s) added.")
        correlation.extend(
            _local_file_summary(evidence) for evidence in session.evidence.local_files
        )
    if session.evidence.pasted_logs:
        correlation.append(f"{len(session.evidence.pasted_logs)} pasted log excerpt(s) added.")
        correlation.extend(
            _pasted_evidence_summary(evidence) for evidence in session.evidence.pasted_logs
        )

    unknowns = _unknowns_for(session)
    next_steps = _next_steps_for(session)
    suggested_internal_note = _suggested_note(
        session,
        summary,
        likely_root_cause,
        unknowns,
        next_steps,
    )

    assessment = Assessment(
        summary=summary,
        likely_root_cause=likely_root_cause,
        confidence=confidence,
        correlation=correlation,
        unknowns=unknowns,
        next_steps=next_steps,
        suggested_internal_note=suggested_internal_note,
    )
    session.assessment = assessment
    return assessment


def session_to_report(session: InvestigationSession) -> TriageReport:
    """Convert an investigation session to the existing report schema."""
    assessment = session.assessment or assess_session(session)
    report = TriageReport(
        finding=assessment.likely_root_cause,
        confidence=assessment.confidence,
        evidence=_report_evidence(session),
        suggested_note=assessment.suggested_internal_note,
        next_checks=assessment.next_steps,
        unknowns=assessment.unknowns,
        ticket_id=session.ticket.id,
        site_name="unknown",
        window=_time_window_for(session),
        sources=_sources_for(session),
        log_event_count=0,
        generated_at=datetime.now(UTC),
    )
    session.report = report
    return report


class _AttachmentEvent:
    def __init__(self, evidence: AttachmentEvidence, event: TimelineEvent) -> None:
        self.evidence = evidence
        self.event = event


def _comment_event(comment: Any, index: int) -> TimelineEvent:
    created_at = _value(comment, "created_at")
    author = _value(comment, "author") or "unknown author"
    body = _value(comment, "body") or ""
    visibility = "public" if _value(comment, "is_public") else "internal"
    return TimelineEvent(
        timestamp=created_at if isinstance(created_at, datetime) else None,
        source="comments",
        kind="comment",
        message=f"{visibility} comment from {author}: {_first_line(body)}",
        raw_ref=f"comment:{index}",
    )


def _attachments_from_comment(comment: Any) -> list[_AttachmentEvent]:
    attachments = _value(comment, "attachments") or []
    discovered: list[_AttachmentEvent] = []
    for index, attachment in enumerate(attachments):
        filename = (
            _value(attachment, "filename")
            or _value(attachment, "file_name")
            or _value(attachment, "name")
            or "attachment"
        )
        content_type = _value(attachment, "content_type") or _value(attachment, "contentType")
        size_bytes = _value(attachment, "size_bytes")
        size = size_bytes if size_bytes is not None else _value(attachment, "size")
        timestamp = _value(attachment, "created_at") or _value(attachment, "updated_at")
        evidence = AttachmentEvidence(
            filename=str(filename),
            content_type=str(content_type) if content_type is not None else None,
            size_bytes=int(size) if size is not None else None,
        )
        event = _attachment_event(evidence, index, timestamp)
        discovered.append(_AttachmentEvent(evidence, event))
    return discovered


def _attachment_event(
    evidence: AttachmentEvidence,
    index: int,
    timestamp: Any = None,
) -> TimelineEvent:
    return TimelineEvent(
        timestamp=timestamp if isinstance(timestamp, datetime) else None,
        source="attachments",
        kind="attachment",
        message=f"Attachment found: {evidence.filename}",
        raw_ref=f"attachment:{index}",
    )


def _value(source: Any, key: str) -> Any:
    if isinstance(source, dict):
        return source.get(key)
    return getattr(source, key, None)


def _detect_file_type(path: Path) -> str:
    try:
        content = path.read_bytes()
    except OSError:
        return "unknown"
    if b"\x00" in content:
        return "unknown"
    try:
        decoded = content.decode("utf-8")
    except UnicodeDecodeError:
        return "unknown"

    suffix = path.suffix.lower()
    if suffix == ".log":
        return "log"
    if suffix == ".json":
        return "json"
    if suffix in {".txt", ".text", ".md", ".csv"}:
        return "text"

    stripped = decoded.strip()
    if stripped.startswith(("{", "[")):
        try:
            json.loads(decoded)
        except json.JSONDecodeError:
            return "text"
        return "json"
    return "text" if decoded else "unknown"


def _read_text_if_supported(path: Path, detected_type: str) -> str | None:
    if detected_type not in {"text", "log", "json"}:
        return None
    try:
        return path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return None


def _sources_for(session: InvestigationSession) -> list[str]:
    sources = ["zendesk"]
    if session.evidence.comments:
        sources.append("comments")
    if session.evidence.attachments:
        sources.append("attachments")
    if session.evidence.local_files:
        sources.append("local_files")
    if session.evidence.pasted_logs:
        sources.append("pasted_logs")
    sources.extend(session.evidence.optional_sources)
    return list(dict.fromkeys(sources))


def _unknowns_for(session: InvestigationSession) -> list[str]:
    unknowns = [
        "Attachment download/extraction is future work; attachment contents were not ingested.",
    ]
    if not session.evidence.attachments:
        unknowns.append("No Zendesk attachment metadata was available.")
    if not session.evidence.local_files:
        unknowns.append("No local log files were provided.")
    if not session.evidence.pasted_logs:
        unknowns.append("No pasted log excerpts were provided.")
    return unknowns


def _next_steps_for(session: InvestigationSession) -> list[str]:
    next_steps = ["Confirm customer impact window and affected workstation/station."]
    if session.evidence.attachments:
        next_steps.append(
            "Download and extract Zendesk attachments when attachment ingestion is available.",
        )
    if not session.evidence.local_files:
        next_steps.append(
            "Collect relevant station, workstation, or service logs as local evidence.",
        )
    if session.evidence.pasted_logs:
        next_steps.append("Correlate pasted log timestamps with the ticket comment timeline.")
    return next_steps


def _suggested_note(
    session: InvestigationSession,
    summary: str,
    likely_root_cause: str,
    unknowns: list[str],
    next_steps: list[str],
) -> str:
    lines = [
        f"Zendesk ticket #{session.ticket.id} investigation draft",
        "",
        f"Summary: {summary}",
        f"Likely root cause: {likely_root_cause}",
    ]
    evidence_summaries = _supplemental_evidence_summaries(session)
    if evidence_summaries:
        lines.extend(
            [
                "",
                "Supplemental evidence reviewed:",
                *[f"- {summary}" for summary in evidence_summaries],
            ],
        )
    lines.extend(
        [
            "",
            "Unknowns:",
            *[f"- {unknown}" for unknown in unknowns],
            "",
            "Next steps:",
            *[f"- {step}" for step in next_steps],
        ],
    )
    return "\n".join(lines)


def _supplemental_evidence_summaries(session: InvestigationSession) -> list[str]:
    summaries = [_local_file_summary(evidence) for evidence in session.evidence.local_files]
    summaries.extend(
        _pasted_evidence_summary(evidence) for evidence in session.evidence.pasted_logs
    )
    return summaries


def _local_file_summary(evidence: LocalFileEvidence) -> str:
    metadata = (
        f"Local file {evidence.path} "
        f"({evidence.detected_type or 'unknown'}, {_format_bytes(evidence.size_bytes)})"
    )
    excerpt = _evidence_excerpt(evidence.extracted_text)
    if excerpt is None:
        return f"{metadata}; no text extracted."
    return f"{metadata}: {excerpt}"


def _pasted_evidence_summary(evidence: PastedEvidence) -> str:
    excerpt = _evidence_excerpt(evidence.text)
    if excerpt is None:
        return f"Pasted evidence {evidence.label}: (empty)"
    return f"Pasted evidence {evidence.label}: {excerpt}"


def _evidence_excerpt(text: str | None, limit: int = EVIDENCE_EXCERPT_LIMIT) -> str | None:
    if text is None:
        return None
    normalized = " ".join(text.split())
    if not normalized:
        return None
    if len(normalized) <= limit:
        return normalized
    return f"{normalized[:limit].rstrip()} [truncated]"


def _format_bytes(size_bytes: int | None) -> str:
    if size_bytes is None:
        return "unknown size"
    return f"{size_bytes} bytes"


def _report_evidence(session: InvestigationSession) -> list[EvidenceItem]:
    items = [
        EvidenceItem(
            timestamp=session.ticket.created_at,
            service="zendesk",
            message=f"Ticket created: {session.ticket.subject}",
        ),
    ]
    items.extend(
        EvidenceItem(
            timestamp=event.timestamp,
            service=event.source,
            message=event.message,
        )
        for event in session.timeline
        if event.kind != "ticket_created"
    )
    items.extend(
        EvidenceItem(
            service="local_files",
            message=_local_file_summary(evidence),
        )
        for evidence in session.evidence.local_files
    )
    items.extend(
        EvidenceItem(
            service="pasted_logs",
            message=_pasted_evidence_summary(evidence),
        )
        for evidence in session.evidence.pasted_logs
    )
    return items


def _time_window_for(session: InvestigationSession) -> TimeWindow:
    timestamps = [session.ticket.created_at, session.ticket.updated_at]
    timestamps.extend(event.timestamp for event in session.timeline if event.timestamp is not None)
    return TimeWindow(start=min(timestamps), end=max(timestamps))


def _sort_timeline(timeline: list[TimelineEvent]) -> None:
    timeline.sort(key=lambda event: event.timestamp or datetime.max.replace(tzinfo=UTC))


def _first_line(text: str) -> str:
    line = text.strip().splitlines()[0] if text.strip() else ""
    return line[:160]
