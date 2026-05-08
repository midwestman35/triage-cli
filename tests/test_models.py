"""Tests for TriageReport schema and related models."""
from __future__ import annotations

from datetime import UTC, datetime, timedelta, timezone
from pathlib import Path

import pytest
from pydantic import ValidationError

from triage_cli.models import (
    Assessment,
    AttachmentEvidence,
    Comment,
    EvidenceItem,
    InvestigationEvidence,
    InvestigationSession,
    LLMTriageOutput,
    LocalFileEvidence,
    PastedEvidence,
    Ticket,
    TimelineEvent,
    TimeWindow,
    TriageReport,
)


def test_llm_triage_output_round_trip():
    payload = {
        "finding": "Station CH-22 may be failing SIP auth.",
        "confidence": "medium",
        "evidence": [
            {
                "timestamp": "2026-05-07T14:03:12Z",
                "service": "auth-service",
                "message": "401 Unauthorized",
            },
        ],
        "suggested_note": "Reviewed Datadog logs...",
        "next_checks": ["Verify station credentials"],
        "unknowns": ["Whether config changed"],
    }
    out = LLMTriageOutput.model_validate(payload)
    assert out.confidence == "medium"
    assert len(out.evidence) == 1
    assert out.evidence[0].service == "auth-service"

    # JSON round-trip preserves data.
    out2 = LLMTriageOutput.model_validate_json(out.model_dump_json())
    assert out2.finding == out.finding
    assert out2.next_checks == ["Verify station credentials"]


def test_llm_triage_output_rejects_bad_confidence():
    with pytest.raises(ValidationError):
        LLMTriageOutput.model_validate(
            {
                "finding": "x",
                "confidence": "panic",
                "evidence": [],
                "suggested_note": "y",
            },
        )


def test_llm_triage_output_defaults_optional_arrays():
    out = LLMTriageOutput.model_validate(
        {
            "finding": "x",
            "confidence": "low",
            "evidence": [],
            "suggested_note": "y",
        },
    )
    assert out.next_checks == []
    assert out.unknowns == []


def test_evidence_item_optional_fields():
    e = EvidenceItem(message="from ticket text")
    assert e.timestamp is None
    assert e.service is None


def test_triage_report_extends_llm_output():
    now = datetime.now(UTC)
    report = TriageReport(
        finding="x",
        confidence="low",
        evidence=[],
        suggested_note="y",
        ticket_id=12345,
        site_name="chicago-pd",
        window=TimeWindow(start=now, end=now),
        sources=["zendesk"],
        log_event_count=0,
        generated_at=now,
    )
    assert report.ticket_id == 12345
    assert report.confidence == "low"
    # LLM-side defaults still work.
    assert report.next_checks == []
    assert report.unknowns == []


def test_triage_report_json_round_trip():
    now = datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    later = datetime(2026, 5, 7, 14, 15, 0, tzinfo=UTC)
    report = TriageReport(
        finding="x",
        confidence="medium",
        evidence=[
            EvidenceItem(timestamp=now, service="sip-edge", message="INVITE retry"),
        ],
        suggested_note="y",
        next_checks=["check creds"],
        unknowns=[],
        ticket_id=42,
        site_name="aurora-pd",
        window=TimeWindow(start=now, end=later),
        sources=["zendesk", "datadog"],
        log_event_count=8,
        generated_at=later,
    )
    json_str = report.model_dump_json()
    restored = TriageReport.model_validate_json(json_str)
    assert restored.ticket_id == 42
    assert restored.window.end == later
    assert restored.evidence[0].timestamp == now


def test_report_datetimes_normalize_to_utc():
    offset = timezone(timedelta(hours=-6))
    local = datetime(2026, 5, 7, 8, 0, 0, tzinfo=offset)
    naive = datetime(2026, 5, 7, 14, 15, 0)
    report = TriageReport(
        finding="x",
        confidence="low",
        evidence=[],
        suggested_note="y",
        ticket_id=42,
        site_name="aurora-pd",
        window=TimeWindow(start=local, end=naive),
        sources=["zendesk"],
        log_event_count=0,
        generated_at=local,
    )
    assert report.window.start == datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    assert report.window.end == datetime(2026, 5, 7, 14, 15, 0, tzinfo=UTC)
    assert report.generated_at == datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)


def test_investigation_models_round_trip_and_defaults():
    now = datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    ticket = Ticket(
        id=42,
        subject="Audio dropouts",
        description="Caller audio drops after reboot.",
        created_at=now,
        updated_at=now,
    )
    evidence = InvestigationEvidence(
        ticket_id=42,
        attachments=[
            AttachmentEvidence(
                filename="station_logs.zip",
                content_type="application/zip",
                size_bytes=2048,
            ),
        ],
        local_files=[
            LocalFileEvidence(
                path="/tmp/station.log",
                size_bytes=11,
                detected_type="log",
                extracted_text="line one",
            ),
        ],
        pasted_logs=[PastedEvidence(label="console", text="WARN audio dropped")],
        optional_sources=["datadog"],
    )
    assessment = Assessment(
        summary="Ticket evidence reviewed.",
        likely_root_cause="Insufficient evidence for a specific root cause.",
        confidence="low",
        correlation=["Ticket description mentions audio dropouts."],
        unknowns=["No station logs were provided."],
        next_steps=["Collect workstation logs."],
        suggested_internal_note="Reviewed ticket evidence.",
    )
    session = InvestigationSession(
        ticket=ticket,
        evidence=evidence,
        timeline=[
            TimelineEvent(
                timestamp=now,
                source="zendesk",
                kind="ticket_created",
                message="Ticket created: Audio dropouts",
            ),
        ],
        assessment=assessment,
    )

    restored = InvestigationSession.model_validate_json(session.model_dump_json())

    assert restored.evidence.attachments[0].source == "zendesk_attachment"
    assert restored.evidence.attachments[0].local_path is None
    assert restored.evidence.attachments[0].extracted_text is None
    assert restored.evidence.optional_sources == ["datadog"]
    assert restored.assessment is not None
    assert restored.assessment.confidence == "low"


def test_investigation_evidence_comments_are_comment_models():
    now = datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)

    evidence = InvestigationEvidence(
        ticket_id=42,
        comments=[
            {
                "author": "Agent One",
                "body": "Customer reports intermittent audio loss.",
                "created_at": now,
                "is_public": False,
            },
        ],
    )

    assert evidence.comments == [
        Comment(
            author="Agent One",
            body="Customer reports intermittent audio loss.",
            created_at=now,
            is_public=False,
        ),
    ]


def test_attachment_evidence_local_path_is_path():
    path = Path("/tmp/station_logs.zip")

    evidence = AttachmentEvidence(filename="station_logs.zip", local_path=path)
    restored = AttachmentEvidence.model_validate_json(evidence.model_dump_json())

    assert evidence.local_path == path
    assert isinstance(evidence.local_path, Path)
    assert restored.local_path == path
    assert isinstance(restored.local_path, Path)


def test_local_file_evidence_path_is_intentionally_path():
    path = Path("/tmp/station.log")

    evidence = LocalFileEvidence(path=path)
    restored = LocalFileEvidence.model_validate_json(evidence.model_dump_json())

    assert evidence.path == path
    assert isinstance(evidence.path, Path)
    assert restored.path == path
    assert isinstance(restored.path, Path)
