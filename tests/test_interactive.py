"""Tests for the interactive evidence-collection orchestration."""
from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path

import pytest

from triage_cli.models import AttachmentEvidence, Comment, Ticket


def _ticket_with_attachments(attachments: list[AttachmentEvidence]) -> Ticket:
    ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    return Ticket(
        id=44496, subject="x", description="y",
        created_at=ts, updated_at=ts,
        comments=[
            Comment(
                author="agent", body="msg", created_at=ts,
                is_public=True, attachments=attachments,
            ),
        ],
    )


class _FakeZendesk:
    """Minimal fake of ZendeskClient.download_attachment for tests."""

    def __init__(self, payloads: dict[str, bytes]) -> None:
        self.payloads = payloads
        self.calls: list[tuple[str, Path]] = []

    def download_attachment(
        self, url: str, dest: Path, *, max_bytes: int = 0,
    ) -> tuple[int, str]:
        import hashlib
        body = self.payloads[url]
        dest.write_bytes(body)
        self.calls.append((url, dest))
        return len(body), hashlib.sha256(body).hexdigest()


def test_download_attachments_user_yes_downloads_all(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """User confirms; both attachments are downloaded; manifest updated."""
    from triage_cli.interactive import (
        download_attachments,
        ensure_workspace,
        read_manifest,
    )

    ws = ensure_workspace(tmp_path, ticket_id=44496)

    attachments_meta = [
        AttachmentEvidence(
            filename="log.txt",
            content_type="text/plain",
            size_bytes=11,
            content_url="https://zd/log",
        ),
        AttachmentEvidence(
            filename="evt.pdf",
            content_type="application/pdf",
            size_bytes=8,
            content_url="https://zd/pdf",
        ),
    ]
    ticket = _ticket_with_attachments(attachments_meta)
    fake_zd = _FakeZendesk({
        "https://zd/log": b"hello world",
        "https://zd/pdf": b"%PDF-1.4",
    })

    # Mock the y/n prompt to always say yes.
    monkeypatch.setattr("triage_cli.interactive.confirm_download", lambda _ticket: True)

    result = download_attachments(ticket, fake_zd, ws)

    assert len(result) == 2
    assert all(a.local_path is not None for a in result)
    assert (ws.attachments_dir / "log.txt").exists()
    assert (ws.attachments_dir / "evt.pdf").exists()

    manifest = read_manifest(ws.attachments_dir)
    assert "log.txt" in manifest
    assert "evt.pdf" in manifest


def test_download_attachments_user_no_returns_metadata_only(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """User declines; no downloads; AttachmentEvidence has no local_path."""
    from triage_cli.interactive import download_attachments, ensure_workspace

    ws = ensure_workspace(tmp_path, ticket_id=44496)
    attachments_meta = [
        AttachmentEvidence(
            filename="log.txt", content_type="text/plain",
            size_bytes=11, content_url="https://zd/log",
        ),
    ]
    ticket = _ticket_with_attachments(attachments_meta)
    fake_zd = _FakeZendesk({"https://zd/log": b"hello"})

    monkeypatch.setattr("triage_cli.interactive.confirm_download", lambda _ticket: False)

    result = download_attachments(ticket, fake_zd, ws)

    assert len(result) == 1
    assert result[0].local_path is None
    assert fake_zd.calls == []


def test_download_attachments_skips_already_downloaded(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Re-run: manifest match → no GET issued; existing local_path preserved."""
    from triage_cli.interactive import (
        download_attachments,
        ensure_workspace,
        write_manifest_entry,
    )

    ws = ensure_workspace(tmp_path, ticket_id=44496)
    (ws.attachments_dir / "log.txt").write_text("hello world", encoding="utf-8")
    write_manifest_entry(
        ws.attachments_dir, filename="log.txt", size=11, sha256="abc",
    )

    attachments_meta = [
        AttachmentEvidence(
            filename="log.txt", content_type="text/plain",
            size_bytes=11, content_url="https://zd/log",
        ),
    ]
    ticket = _ticket_with_attachments(attachments_meta)
    fake_zd = _FakeZendesk({"https://zd/log": b"hello world"})

    monkeypatch.setattr("triage_cli.interactive.confirm_download", lambda _ticket: True)

    result = download_attachments(ticket, fake_zd, ws)

    assert len(result) == 1
    assert result[0].local_path == ws.attachments_dir / "log.txt"
    assert fake_zd.calls == []  # no actual download issued


def test_download_attachments_no_attachments_returns_empty(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Ticket with zero attachments → empty list, no prompt issued."""
    from triage_cli.interactive import download_attachments, ensure_workspace

    ws = ensure_workspace(tmp_path, ticket_id=44496)
    ticket = _ticket_with_attachments([])
    fake_zd = _FakeZendesk({})

    # Prompt should not be called; if it is, this raises and test fails.
    def boom(_ticket):
        raise AssertionError("confirm_download should not be called for empty list")

    monkeypatch.setattr("triage_cli.interactive.confirm_download", boom)

    result = download_attachments(ticket, fake_zd, ws)
    assert result == []
