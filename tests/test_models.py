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


def test_attachment_evidence_accepts_content_url():
    """content_url is optional; when set it round-trips through Pydantic."""
    a = AttachmentEvidence(
        filename="log.txt",
        content_type="text/plain",
        size_bytes=1024,
        content_url="https://example.zendesk.com/attachments/token/abc/log.txt",
    )
    assert a.content_url == "https://example.zendesk.com/attachments/token/abc/log.txt"


def test_attachment_evidence_content_url_defaults_none():
    """Existing call sites that omit content_url keep working."""
    a = AttachmentEvidence(filename="log.txt")
    assert a.content_url is None


def test_attachment_evidence_excludes_content_url_when_dumped_with_exclude():
    """Render layer can scrub the URL before persisting JSON to disk."""
    a = AttachmentEvidence(
        filename="log.txt",
        content_url="https://example.zendesk.com/attachments/token/abc/log.txt",
    )
    dumped = a.model_dump(exclude={"content_url"})
    assert "content_url" not in dumped


def test_triage_bundle_evidence_fields_default_empty():
    """All three evidence lists default to []; constructing a bundle without
    them still works (watcher path)."""
    from triage_cli.models import (
        AnchorSource, SiteEntry, Ticket, TriageBundle,
    )

    ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    bundle = TriageBundle(
        ticket=Ticket(
            id=1, subject="x", description="y",
            created_at=ts, updated_at=ts, comments=[],
        ),
        site_entry=SiteEntry(
            friendly_name="Aurora 911, CO",
            site_name="us-co-aurora-apex",
            cnc="abc",
        ),
        anchor=ts,
        anchor_source=AnchorSource.CREATED_AT,
        window_start=ts,
        window_end=ts,
    )

    assert bundle.downloaded_attachments == []
    assert bundle.local_files == []
    assert bundle.pasted_logs == []


def test_truncate_head_tail_short_content_unchanged():
    """Content under the cap is returned verbatim — no marker added."""
    from triage_cli.models import truncate_head_tail

    text = "small log\n" * 5
    result = truncate_head_tail(text, head_bytes=1000, tail_bytes=500)
    assert result == text


def test_truncate_head_tail_long_content_keeps_head_and_tail():
    """Long content keeps head_bytes from the front, tail_bytes from the back,
    and inserts a [truncated N bytes] marker between them."""
    from triage_cli.models import truncate_head_tail

    head = "A" * 100
    middle = "B" * 1000
    tail = "C" * 50
    text = head + middle + tail

    result = truncate_head_tail(text, head_bytes=100, tail_bytes=50)
    assert result.startswith(head)
    assert result.endswith(tail)
    assert "[truncated 1000 bytes]" in result
    assert "B" not in result  # middle is excised entirely


def test_truncate_head_tail_exact_cap_no_marker():
    """At exactly head + tail, no truncation marker is inserted."""
    from triage_cli.models import truncate_head_tail

    text = "X" * 150
    result = truncate_head_tail(text, head_bytes=100, tail_bytes=50)
    assert result == text
    assert "[truncated" not in result


def test_truncate_head_tail_zero_tail_does_not_duplicate():
    """tail_bytes=0 must produce empty tail, not the full encoded string.

    Regression: encoded[-0:] in Python is the entire slice, not empty. The
    function must guard against this so callers that ask for head-only get
    head-only.
    """
    from triage_cli.models import truncate_head_tail

    text = "A" * 100 + "B" * 100  # 200 bytes
    result = truncate_head_tail(text, head_bytes=50, tail_bytes=0)
    # head: 50 A's; truncated marker; empty tail
    assert result.startswith("A" * 50)
    assert "[truncated 150 bytes]" in result
    assert "B" not in result
    # And the tail position is empty (not 200 bytes of duplicate text).
    assert not result.endswith("A" * 100)


def test_truncate_head_tail_zero_head_returns_only_tail():
    """head_bytes=0 produces only the tail portion."""
    from triage_cli.models import truncate_head_tail

    text = "A" * 100 + "B" * 100
    result = truncate_head_tail(text, head_bytes=0, tail_bytes=50)
    assert result.endswith("B" * 50)
    assert "[truncated 150 bytes]" in result
    assert "A" not in result


def _bundle_with(downloaded=None, local=None, pasted=None):
    """Helper to build a minimal bundle with chosen evidence fields."""
    from triage_cli.models import (
        AnchorSource, SiteEntry, Ticket, TriageBundle,
    )

    ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    return TriageBundle(
        ticket=Ticket(
            id=1, subject="x", description="y",
            created_at=ts, updated_at=ts, comments=[],
        ),
        site_entry=SiteEntry(
            friendly_name="Aurora 911, CO",
            site_name="us-co-aurora-apex",
            cnc="abc",
        ),
        anchor=ts,
        anchor_source=AnchorSource.CREATED_AT,
        window_start=ts,
        window_end=ts,
        downloaded_attachments=downloaded or [],
        local_files=local or [],
        pasted_logs=pasted or [],
    )


def test_as_user_message_no_evidence_section_when_all_empty():
    """Headless triage produces a bundle with no evidence; the prompt should
    not include a Supplemental Evidence header."""
    bundle = _bundle_with()
    out = bundle.as_user_message()
    assert "Supplemental Evidence" not in out


def test_as_user_message_renders_local_text_file_with_content():
    """A short text file is inlined verbatim; the section header appears."""
    local = [
        LocalFileEvidence(
            path=Path("/tmp/apex.log"),
            size_bytes=20,
            detected_type="log",
            extracted_text="boot ok\nerror at 3am\n",
        ),
    ]
    bundle = _bundle_with(local=local)
    out = bundle.as_user_message()
    assert "# Supplemental Evidence" in out
    assert "apex.log" in out
    assert "boot ok" in out
    assert "error at 3am" in out


def test_as_user_message_renders_binary_attachment_metadata_only():
    """Binary attachment lists name/size/type but no bytes."""
    downloaded = [
        AttachmentEvidence(
            filename="evt.pdf",
            content_type="application/pdf",
            size_bytes=4_000_000,
            local_path=Path("/tmp/evt.pdf"),
            extracted_text=None,
        ),
    ]
    bundle = _bundle_with(downloaded=downloaded)
    out = bundle.as_user_message()
    assert "evt.pdf" in out
    assert "application/pdf" in out
    # Binary file: no text content, just a tag indicating it's not extracted.
    assert "(binary, not extracted)" in out


def test_as_user_message_renders_pasted_evidence():
    pasted = [PastedEvidence(label="SIP_TRACE", text="INVITE sip:foo")]
    bundle = _bundle_with(pasted=pasted)
    out = bundle.as_user_message()
    assert "SIP_TRACE" in out
    assert "INVITE sip:foo" in out


def test_as_user_message_truncates_oversized_text():
    """Large extracted_text is bounded by EVIDENCE_HEAD_BYTES + EVIDENCE_TAIL_BYTES."""
    from triage_cli.models import EVIDENCE_HEAD_BYTES, EVIDENCE_TAIL_BYTES

    huge = "X" * (EVIDENCE_HEAD_BYTES + EVIDENCE_TAIL_BYTES + 5000)
    local = [
        LocalFileEvidence(
            path=Path("/tmp/big.log"),
            size_bytes=len(huge),
            detected_type="log",
            extracted_text=huge,
        ),
    ]
    bundle = _bundle_with(local=local)
    out = bundle.as_user_message()
    assert "[truncated 5000 bytes]" in out


def test_as_user_message_no_trailing_whitespace_only_lines():
    """Evidence text ending in \\n must not produce whitespace-only lines.

    Regression: indent_continuations replaces every \\n (including a trailing
    one) with \\n  , which after join produces lines containing only two
    spaces. The render helpers must strip trailing newlines before indenting.
    """
    local = [
        LocalFileEvidence(
            path=Path("/tmp/apex.log"),
            size_bytes=20,
            detected_type="log",
            extracted_text="boot ok\nerror at 3am\n",  # trailing \n
        ),
    ]
    pasted = [PastedEvidence(label="SIP", text="INVITE sip:foo\n")]
    bundle = _bundle_with(local=local, pasted=pasted)
    out = bundle.as_user_message()

    # No line should consist only of whitespace.
    for line in out.split("\n"):
        if line:  # non-empty lines must have non-whitespace content
            assert line.strip(), f"whitespace-only line found: {line!r}"
