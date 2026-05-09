"""Tests for ZendeskClient.download_attachment streaming and size enforcement."""
from __future__ import annotations

import hashlib
from pathlib import Path
from typing import Any

import httpx
import pytest

from triage_cli.zendesk import ZendeskClient


def _client() -> ZendeskClient:
    return ZendeskClient(subdomain="example", email="e@x.com", api_token="tok")


def _stream_response(content: bytes, status: int = 200) -> httpx.Response:
    """Build an httpx.Response that behaves like a stream when iterated."""
    return httpx.Response(status, content=content)


def test_download_attachment_happy_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Stream a 4KB body to disk; bytes_written and sha256 are correct."""
    payload = b"ABC" * 1500  # 4500 bytes
    expected_sha = hashlib.sha256(payload).hexdigest()

    def fake_stream(self: httpx.Client, method: str, url: str, **_kw: Any):  # noqa: ARG001
        # The real client.stream() returns a context manager that yields a Response
        # whose iter_bytes() yields chunks. Mirror that interface.
        class _StreamCtx:
            def __enter__(_inner) -> httpx.Response:
                return _stream_response(payload)
            def __exit__(_inner, *args: Any) -> None:
                return None
        return _StreamCtx()

    monkeypatch.setattr(httpx.Client, "stream", fake_stream)

    dest = tmp_path / "log.txt"
    with _client() as zd:
        bytes_written, sha = zd.download_attachment(
            "https://example.zendesk.com/attachments/abc/log.txt",
            dest,
            max_bytes=10_000,
        )

    assert bytes_written == 4500
    assert sha == expected_sha
    assert dest.read_bytes() == payload
