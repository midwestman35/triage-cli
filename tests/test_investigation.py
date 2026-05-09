"""Tests for guided investigation evidence/session behavior."""
from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path
from types import SimpleNamespace

from triage_cli.investigation import (
    add_local_file,
    add_pasted_evidence,
    assess_session,
    build_timeline,
    create_session,
    session_to_report,
)
from triage_cli.models import (
    AttachmentEvidence,
    Comment,
    InvestigationEvidence,
    InvestigationSession,
    Ticket,
    TriageReport,
)


def _ticket(*, comments: list[Comment] | None = None) -> Ticket:
    created = datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    updated = datetime(2026, 5, 7, 14, 30, 0, tzinfo=UTC)
    return Ticket(
        id=123,
        subject="Audio drops on dispatch console",
        description="Caller audio drops after console reboot.",
        requester_org=None,
        tags=[],
        created_at=created,
        updated_at=updated,
        comments=comments
        if comments is not None
        else [
            Comment(
                author="Agent One",
                body="Customer reports intermittent audio loss.",
                created_at=datetime(2026, 5, 7, 14, 5, 0, tzinfo=UTC),
                is_public=False,
            ),
        ],
    )


def test_create_session_includes_ticket_comments_and_baseline_timeline():
    session = create_session(_ticket())

    assert session.ticket.id == 123
    assert session.evidence.ticket_id == 123
    assert len(session.evidence.comments) == 1
    assert isinstance(session.evidence.comments[0], Comment)
    assert [event.kind for event in session.timeline] == ["ticket_created", "comment"]
    assert session.timeline[0].message == "Ticket created: Audio drops on dispatch console"
    assert "Customer reports intermittent audio loss." in session.timeline[1].message


def test_create_session_captures_attachment_metadata_without_downloading():
    comment = SimpleNamespace(
        author="Agent One",
        body="Logs attached.",
        created_at=datetime(2026, 5, 7, 14, 5, 0, tzinfo=UTC),
        is_public=False,
        attachments=[
            {
                "file_name": "station_logs.zip",
                "content_type": "application/zip",
                "size": 4096,
                "created_at": datetime(2026, 5, 7, 14, 6, 0, tzinfo=UTC),
            },
        ],
    )

    ticket = Ticket.model_construct(
        id=123,
        subject="Audio drops on dispatch console",
        description="Caller audio drops after console reboot.",
        requester_org=None,
        tags=[],
        created_at=datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC),
        updated_at=datetime(2026, 5, 7, 14, 30, 0, tzinfo=UTC),
        comments=[comment],
    )

    session = create_session(ticket)

    attachment = session.evidence.attachments[0]
    assert attachment.filename == "station_logs.zip"
    assert attachment.content_type == "application/zip"
    assert attachment.size_bytes == 4096
    assert attachment.source == "zendesk_attachment"
    assert attachment.local_path is None
    assert attachment.extracted_text is None
    assert session.timeline[-1].kind == "attachment"
    assert session.timeline[-1].timestamp == datetime(2026, 5, 7, 14, 6, 0, tzinfo=UTC)


def test_create_session_captures_validated_comment_attachment_metadata():
    comment = Comment(
        author="Agent One",
        body="Logs attached.",
        created_at=datetime(2026, 5, 7, 14, 5, 0, tzinfo=UTC),
        is_public=False,
        attachments=[
            AttachmentEvidence(
                filename="station_logs.zip",
                content_type="application/zip",
                size_bytes=4096,
            ),
        ],
    )

    session = create_session(_ticket(comments=[comment]))

    assert session.evidence.attachments == comment.attachments
    assert [event.kind for event in session.timeline] == [
        "ticket_created",
        "comment",
        "attachment",
    ]
    assert session.timeline[-1].message == "Attachment found: station_logs.zip"
    assert session.timeline[-1].raw_ref == "attachment:0"


def test_add_local_log_file_records_metadata_text_and_timeline(tmp_path: Path):
    log_path = tmp_path / "station.log"
    log_text = "2026-05-07T14:10:00Z WARN audio dropped\n"
    log_path.write_text(log_text, encoding="utf-8")
    session = create_session(_ticket())

    evidence = add_local_file(session, log_path)

    assert evidence.path == log_path
    assert evidence.size_bytes == len(log_text.encode())
    assert evidence.detected_type == "log"
    assert evidence.extracted_text == log_text
    assert session.evidence.local_files == [evidence]
    assert session.timeline[-1].source == "local_files"
    assert session.timeline[-1].kind == "local_file"
    assert str(log_path) in session.timeline[-1].message


def test_binary_log_file_remains_metadata_only_in_reports(tmp_path: Path):
    binary_content = b"\x00BINARY-AUDIO-CAPTURE\xff"
    log_path = tmp_path / "capture.log"
    log_path.write_bytes(binary_content)
    session = create_session(_ticket())

    evidence = add_local_file(session, log_path)
    assessment = assess_session(session)
    report = session_to_report(session)

    report_messages = "\n".join(item.message for item in report.evidence)
    assert evidence.detected_type == "unknown"
    assert evidence.extracted_text is None
    assert "BINARY-AUDIO-CAPTURE" not in report_messages
    assert "BINARY-AUDIO-CAPTURE" not in assessment.suggested_internal_note
    assert "Local file" in report_messages
    assert "unknown" in report_messages
    assert "no text extracted" in report_messages
    assert "no text extracted" in assessment.suggested_internal_note


def test_add_pasted_logs_records_label_text_and_timeline():
    session = create_session(_ticket())

    evidence = add_pasted_evidence(session, "console excerpt", "WARN audio dropped")

    assert evidence.label == "console excerpt"
    assert evidence.text == "WARN audio dropped"
    assert session.evidence.pasted_logs == [evidence]
    assert session.timeline[-1].source == "pasted_logs"
    assert session.timeline[-1].kind == "pasted_log"
    assert "console excerpt" in session.timeline[-1].message


def test_assessment_and_report_are_deterministic_without_datadog_or_site(tmp_path: Path):
    session = create_session(_ticket())
    add_pasted_evidence(session, "console excerpt", "WARN audio dropped")
    binary_path = tmp_path / "capture.bin"
    binary_path.write_bytes(b"\x00\x01\x02")
    add_local_file(session, binary_path)

    assessment = assess_session(session)
    report = session_to_report(session)

    assert assessment.confidence == "medium"
    assert "internal note" not in assessment.suggested_internal_note.lower()
    assert "Zendesk ticket #123" in assessment.suggested_internal_note
    assert not any("Attachment download/extraction" in unknown for unknown in assessment.unknowns)
    assert isinstance(report, TriageReport)
    assert report.ticket_id == 123
    assert report.site_name == "unknown"
    assert report.log_event_count == 0
    assert report.sources == ["zendesk", "comments", "local_files", "pasted_logs"]
    assert report.window.start == datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    assert report.window.end == datetime(2026, 5, 7, 14, 30, 0, tzinfo=UTC)
    assert report.finding == assessment.likely_root_cause
    assert report.suggested_note == assessment.suggested_internal_note


def test_unknowns_no_longer_says_attachment_download_is_future_work():
    """Pipeline v2 actually downloads attachments; this stale unknown must go."""
    from datetime import UTC, datetime

    from triage_cli.investigation import _unknowns_for, create_session
    from triage_cli.models import Ticket

    ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    ticket = Ticket(
        id=1, subject="x", description="y",
        created_at=ts, updated_at=ts, comments=[],
    )
    session = create_session(ticket)
    unknowns = _unknowns_for(session)

    assert not any("future work" in u.lower() for u in unknowns)


def test_report_and_assessment_include_local_and_pasted_evidence_text(tmp_path: Path):
    session = create_session(_ticket())
    log_path = tmp_path / "station.log"
    log_path.write_text(
        "2026-05-07T14:10:00Z WARN audio dropped after SIP timeout\n",
        encoding="utf-8",
    )
    add_local_file(session, log_path)
    add_pasted_evidence(
        session,
        "console excerpt",
        "Operator observed silence after dispatch console reboot.",
    )

    assessment = assess_session(session)
    report = session_to_report(session)

    report_messages = "\n".join(item.message for item in report.evidence)
    correlation_text = "\n".join(assessment.correlation)
    assert "WARN audio dropped after SIP timeout" in report_messages
    assert "Operator observed silence after dispatch console reboot" in report_messages
    assert "WARN audio dropped after SIP timeout" in correlation_text
    assert "Operator observed silence after dispatch console reboot" in (
        assessment.suggested_internal_note
    )


def test_report_evidence_bounds_large_local_and_pasted_text(tmp_path: Path):
    session = create_session(_ticket())
    log_path = tmp_path / "station.log"
    local_text = "local-start " + ("local-detail " * 80) + "local-end"
    pasted_text = "paste-start " + ("paste-detail " * 80) + "paste-end"
    log_path.write_text(local_text, encoding="utf-8")
    add_local_file(session, log_path)
    add_pasted_evidence(session, "operator notes", pasted_text)

    report = session_to_report(session)

    report_messages = "\n".join(item.message for item in report.evidence)
    assert "local-start" in report_messages
    assert "paste-start" in report_messages
    assert "local-end" not in report_messages
    assert "paste-end" not in report_messages
    assert "[truncated]" in report_messages
    assert "local-end" not in report.suggested_note
    assert "paste-end" not in report.suggested_note
    assert "[truncated]" in report.suggested_note


def test_build_timeline_preserves_evidence_attachments_after_round_trip():
    session = InvestigationSession(
        ticket=_ticket(comments=[]),
        evidence=InvestigationEvidence(
            ticket_id=123,
            attachments=[
                AttachmentEvidence(
                    filename="station_logs.zip",
                    content_type="application/zip",
                    size_bytes=4096,
                ),
            ],
        ),
    )
    restored = InvestigationSession.model_validate_json(session.model_dump_json())

    timeline = build_timeline(restored)

    attachment_events = [event for event in timeline if event.kind == "attachment"]
    assert len(attachment_events) == 1
    assert attachment_events[0].message == "Attachment found: station_logs.zip"
    assert attachment_events[0].raw_ref == "attachment:0"


def test_session_to_report_dedupes_repeated_optional_sources():
    session = create_session(_ticket())
    session.evidence.optional_sources = ["datadog", "datadog", "comments", "local_files"]

    report = session_to_report(session)

    assert report.sources == ["zendesk", "comments", "datadog", "local_files"]


def test_next_steps_no_longer_says_attachment_ingestion_is_future():
    """Pipeline v2 actually downloads attachments; this stale next-step phrasing must go.

    Companion to Task 18's removal of the same future-tense language from
    _unknowns_for. Any wording that says attachment ingestion is "future" or
    "when available" is now actively misleading.
    """
    from datetime import UTC, datetime

    from triage_cli.investigation import _next_steps_for, create_session
    from triage_cli.models import AttachmentEvidence, Comment, Ticket

    ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    # Use a ticket that has at least one attachment so the relevant branch fires.
    ticket = Ticket(
        id=1, subject="x", description="y",
        created_at=ts, updated_at=ts,
        comments=[
            Comment(
                author="agent", body="msg", created_at=ts, is_public=True,
                attachments=[AttachmentEvidence(filename="log.txt")],
            ),
        ],
    )
    session = create_session(ticket)
    next_steps = _next_steps_for(session)

    # The replacement wording about attachments still appears (the branch fired).
    assert any("attachment" in s.lower() for s in next_steps)
    # But the stale future-tense wording is gone.
    assert not any("when attachment ingestion is available" in s for s in next_steps)
    assert not any("future" in s.lower() for s in next_steps)
