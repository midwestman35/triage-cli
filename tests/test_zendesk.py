"""Tests for triage_cli.zendesk.ZendeskClient."""
from __future__ import annotations

from typing import Any

import httpx
import pytest

from triage_cli.zendesk import ZendeskClient, _to_comment


def _client() -> ZendeskClient:
    return ZendeskClient(subdomain="example", email="e@x.com", api_token="tok")


def test_list_view_ticket_ids_paginates(monkeypatch: pytest.MonkeyPatch) -> None:
    """list_view_ticket_ids walks cursor pagination and returns IDs in order received."""
    pages: list[dict[str, Any]] = [
        {
            "tickets": [{"id": 1}, {"id": 2}],
            "meta": {"has_more": True},
            "links": {"next": "https://example.zendesk.com/api/v2/views/9/tickets.json?page=2"},
        },
        {
            "tickets": [{"id": 3}],
            "meta": {"has_more": False},
            "links": {},
        },
    ]
    page_iter = iter(pages)

    def fake_get(self: httpx.Client, url: str, params: Any = None) -> httpx.Response:  # noqa: ARG001
        return httpx.Response(200, json=next(page_iter))

    monkeypatch.setattr(httpx.Client, "get", fake_get)

    with _client() as zd:
        ids = zd.list_view_ticket_ids(9)

    assert ids == [1, 2, 3]


def test_list_view_ticket_ids_404_message(monkeypatch: pytest.MonkeyPatch) -> None:
    """A 404 from the views endpoint surfaces a view-flavored error message."""

    def fake_get(self: httpx.Client, url: str, params: Any = None) -> httpx.Response:  # noqa: ARG001
        return httpx.Response(404, json={"error": "not found"})

    monkeypatch.setattr(httpx.Client, "get", fake_get)

    with _client() as zd, pytest.raises(RuntimeError, match="View 999 not found"):
        zd.list_view_ticket_ids(999)


def test_list_my_ticket_ids_fetches_current_user_then_assigned_tickets(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """list_my_ticket_ids discovers the authenticated user before reading assignments."""
    calls: list[tuple[str, Any]] = []
    pages: list[dict[str, Any]] = [
        {"user": {"id": 42}},
        {
            "tickets": [{"id": 1001}, {"id": 1002}],
            "meta": {"has_more": True},
            "links": {
                "next": (
                    "https://example.zendesk.com/api/v2/users/42/"
                    "tickets/assigned.json?page%5Bafter%5D=abc"
                )
            },
        },
        {
            "tickets": [{"id": 1003}],
            "meta": {"has_more": False},
            "links": {},
        },
    ]
    page_iter = iter(pages)

    def fake_get(self: httpx.Client, url: str, params: Any = None) -> httpx.Response:  # noqa: ARG001
        calls.append((url, params))
        return httpx.Response(200, json=next(page_iter))

    monkeypatch.setattr(httpx.Client, "get", fake_get)

    with _client() as zd:
        ids = zd.list_my_ticket_ids()

    assert ids == [1001, 1002, 1003]
    assert calls == [
        ("https://example.zendesk.com/api/v2/users/me.json", None),
        (
            "https://example.zendesk.com/api/v2/users/42/tickets/assigned.json",
            {"page[size]": 100},
        ),
        (
            "https://example.zendesk.com/api/v2/users/42/"
            "tickets/assigned.json?page%5Bafter%5D=abc",
            None,
        ),
    ]


def test_list_my_ticket_ids_uses_legacy_next_page_fallback(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """list_my_ticket_ids follows next_page when cursor links are absent."""
    calls: list[tuple[str, Any]] = []
    pages: list[dict[str, Any]] = [
        {"user": {"id": 51}},
        {
            "tickets": [{"id": 7}],
            "next_page": (
                "https://example.zendesk.com/api/v2/users/51/"
                "tickets/assigned.json?page=2"
            ),
        },
        {
            "tickets": [{"id": 8}],
            "next_page": None,
        },
    ]
    page_iter = iter(pages)

    def fake_get(self: httpx.Client, url: str, params: Any = None) -> httpx.Response:  # noqa: ARG001
        calls.append((url, params))
        return httpx.Response(200, json=next(page_iter))

    monkeypatch.setattr(httpx.Client, "get", fake_get)

    with _client() as zd:
        ids = zd.list_my_ticket_ids()

    assert ids == [7, 8]
    assert calls == [
        ("https://example.zendesk.com/api/v2/users/me.json", None),
        (
            "https://example.zendesk.com/api/v2/users/51/tickets/assigned.json",
            {"page[size]": 100},
        ),
        (
            "https://example.zendesk.com/api/v2/users/51/"
            "tickets/assigned.json?page=2",
            None,
        ),
    ]


def test_list_my_ticket_ids_raises_when_current_user_has_no_id(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A malformed /users/me.json response reports the missing user ID clearly."""

    def fake_get(self: httpx.Client, url: str, params: Any = None) -> httpx.Response:  # noqa: ARG001
        return httpx.Response(200, json={"user": {"email": "agent@example.com"}})

    monkeypatch.setattr(httpx.Client, "get", fake_get)

    with _client() as zd, pytest.raises(
        RuntimeError,
        match=r"Could not determine current Zendesk user ID from /users/me\.json",
    ):
        zd.list_my_ticket_ids()


def test_to_comment_maps_attachment_metadata_without_download_fields() -> None:
    raw_comment = {
        "author_id": 7,
        "plain_body": "Logs attached.",
        "created_at": "2026-05-07T14:05:00Z",
        "public": False,
        "attachments": [
            {
                "file_name": "station_logs.zip",
                "content_type": "application/zip",
                "size": 4096,
                "content_url": "https://example.zendesk.com/attachments/1",
            },
        ],
    }

    comment = _to_comment(raw_comment, {7: {"id": 7, "name": "Agent One"}})

    assert comment.attachments[0].filename == "station_logs.zip"
    assert comment.attachments[0].content_type == "application/zip"
    assert comment.attachments[0].size_bytes == 4096
    assert comment.attachments[0].local_path is None
    assert comment.attachments[0].extracted_text is None


def test_attachments_from_raw_preserves_content_url():
    """The download URL must be preserved so the interactive flow can fetch it."""
    from triage_cli.zendesk import _attachments_from_raw

    raw = [
        {
            "file_name": "log.txt",
            "content_type": "text/plain",
            "size": 1024,
            "content_url": "https://example.zendesk.com/attachments/token/abc/log.txt",
        },
    ]
    attachments = _attachments_from_raw(raw)

    assert len(attachments) == 1
    assert attachments[0].filename == "log.txt"
    assert (
        attachments[0].content_url
        == "https://example.zendesk.com/attachments/token/abc/log.txt"
    )


def test_attachments_from_raw_handles_missing_content_url():
    """If Zendesk omits content_url, content_url is None (not an error)."""
    from triage_cli.zendesk import _attachments_from_raw

    raw = [{"file_name": "log.txt", "content_type": "text/plain", "size": 1024}]
    attachments = _attachments_from_raw(raw)
    assert attachments[0].content_url is None
