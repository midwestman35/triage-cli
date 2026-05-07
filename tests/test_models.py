"""Tests for TriageReport schema and related models."""
from __future__ import annotations

from datetime import UTC, datetime, timedelta, timezone

import pytest
from pydantic import ValidationError

from triage_cli.models import (
    EvidenceItem,
    LLMTriageOutput,
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
