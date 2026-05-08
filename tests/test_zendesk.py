"""Tests for triage_cli.zendesk.ZendeskClient."""
from __future__ import annotations

from typing import Any

import httpx
import pytest

from triage_cli.zendesk import ZendeskClient


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


def test_list_attachments_collects_metadata(monkeypatch: pytest.MonkeyPatch) -> None:
    """list_attachments flattens attachment dicts across paginated comments."""
    pages: list[dict[str, Any]] = [
        {
            "comments": [
                {"id": 100, "attachments": [
                    {"file_name": "a.log", "content_type": "text/plain", "size": 10,
                     "content_url": "https://x/a.log"},
                ]},
                {"id": 101, "attachments": []},
            ],
            "meta": {"has_more": True},
            "links": {"next": "https://example.zendesk.com/api/v2/tickets/5/comments.json?p=2"},
        },
        {
            "comments": [
                {"id": 102, "attachments": [
                    {"file_name": "b.zip", "content_type": "application/zip", "size": 200},
                ]},
            ],
            "meta": {"has_more": False},
            "links": {},
        },
    ]
    page_iter = iter(pages)

    def fake_get(self: httpx.Client, url: str, params: Any = None) -> httpx.Response:  # noqa: ARG001
        return httpx.Response(200, json=next(page_iter))

    monkeypatch.setattr(httpx.Client, "get", fake_get)

    with _client() as zd:
        attachments = zd.list_attachments(5)

    assert len(attachments) == 2
    assert attachments[0] == {
        "comment_id": 100,
        "file_name": "a.log",
        "content_type": "text/plain",
        "size": 10,
        "content_url": "https://x/a.log",
    }
    assert attachments[1]["file_name"] == "b.zip"
    assert attachments[1]["comment_id"] == 102
