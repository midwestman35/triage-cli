"""Tests for triage_cli.evidence: ingestion helpers."""
from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path

import pytest

from triage_cli import evidence
from triage_cli.evidence import EvidenceKind, attachments_metadata
from triage_cli.models import Comment, Ticket


def _ticket() -> Ticket:
    ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    return Ticket(
        id=42,
        subject="audio dropouts",
        description="see logs",
        requester_org="Aurora 911, CO",
        tags=["urgent"],
        created_at=ts,
        updated_at=ts,
        comments=[
            Comment(
                author="alice",
                body="started after reboot",
                created_at=datetime(2026, 5, 7, 12, 5, tzinfo=UTC),
                is_public=True,
            ),
            Comment(
                author="bob",
                body="confirmed on console B",
                created_at=datetime(2026, 5, 7, 12, 10, tzinfo=UTC),
                is_public=False,
            ),
        ],
    )


def test_from_ticket_emits_one_event() -> None:
    src, events = evidence.from_ticket(_ticket())
    assert src.kind == EvidenceKind.ZENDESK_TICKET
    assert src.event_count == 1
    assert len(events) == 1
    assert events[0].kind == "ticket_created"
    assert events[0].timestamp == datetime(2026, 5, 7, 12, 0, tzinfo=UTC)


def test_from_comments_one_event_per_comment() -> None:
    src, events = evidence.from_comments(_ticket())
    assert src.kind == EvidenceKind.ZENDESK_COMMENT
    assert src.event_count == 2
    assert {e.attributes["author"] for e in events} == {"alice", "bob"}
    assert {e.attributes["visibility"] for e in events} == {"public", "internal"}


def test_from_local_file_parses_log_lines(tmp_path: Path) -> None:
    p = tmp_path / "syslog.log"
    p.write_text("2026-05-07T12:00:00Z INFO hello\n2026-05-07T12:01:00Z ERROR boom\n")
    src, events = evidence.from_local_file(p)
    assert src.kind == EvidenceKind.LOCAL_FILE
    assert src.event_count == 2
    assert {e.level for e in events} == {"INFO", "ERROR"}


def test_from_local_file_records_unparsed_count(tmp_path: Path) -> None:
    p = tmp_path / "noisy.log"
    p.write_text("garbage\n2026-05-07T12:00:00Z OK\nmore garbage\n")
    src, events = evidence.from_local_file(p)
    assert src.event_count == 1
    assert src.notes is not None
    assert "2 unparsed" in src.notes


def test_from_local_file_missing_raises(tmp_path: Path) -> None:
    with pytest.raises(FileNotFoundError):
        evidence.from_local_file(tmp_path / "does-not-exist.log")


def test_from_local_directory_globs_and_merges(tmp_path: Path) -> None:
    (tmp_path / "a.log").write_text("2026-05-07T12:00:00Z INFO from-a\n")
    (tmp_path / "b.log").write_text("2026-05-07T12:01:00Z INFO from-b\n")
    (tmp_path / "ignored.txt").write_text("2026-05-07T12:02:00Z INFO ignored\n")
    src, events = evidence.from_local_directory(tmp_path)
    assert src.kind == EvidenceKind.LOCAL_DIRECTORY
    assert src.event_count == 2
    assert {e.source for e in events} == {"a.log", "b.log"}


def test_from_pasted_text() -> None:
    text = "2026-05-07T12:00:00Z DEBUG hi\nno-ts\n"
    src, events = evidence.from_pasted_text(text, label="cnc-paste")
    assert src.kind == EvidenceKind.PASTED_TEXT
    assert src.label == "cnc-paste"
    assert src.event_count == 1
    assert src.notes is not None and "1 unparsed" in src.notes


def test_attachments_metadata_is_metadata_only() -> None:
    raw = [
        {"file_name": "logs.zip", "content_type": "application/zip", "size": 1024,
         "content_url": "https://example.com/logs.zip"},
        {"file_name": "screenshot.png", "content_type": "image/png", "size": 512},
    ]
    sources = attachments_metadata(raw, ticket_id=42, comment_id=7)
    assert len(sources) == 2
    assert all(s.kind == EvidenceKind.ZENDESK_ATTACHMENT for s in sources)
    assert all(s.parsed is False for s in sources)
    assert all(s.event_count == 0 for s in sources)
    assert sources[0].extra["content_url"] == "https://example.com/logs.zip"
    assert sources[1].extra["content_type"] == "image/png"
    assert "metadata only" in (sources[0].notes or "")
