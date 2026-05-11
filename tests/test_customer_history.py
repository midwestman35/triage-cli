"""Tests for ZendeskClient.fetch_customer_history."""
from __future__ import annotations

from datetime import UTC, datetime

import pytest


@pytest.fixture()
def mock_zd(monkeypatch):
    """ZendeskClient with _get monkeypatched to avoid real HTTP."""
    from triage_cli.zendesk import ZendeskClient

    client = ZendeskClient.__new__(ZendeskClient)

    def _make_ticket(n: int) -> dict:
        return {
            "id": n,
            "subject": f"Ticket {n}",
            "status": "open",
            "created_at": "2026-05-01T10:00:00Z",
            "updated_at": "2026-05-10T10:00:00Z",
        }

    def fake_get(path, *, params=None, **kwargs):
        if "search.json" in path:
            return {"results": [_make_ticket(i) for i in range(1, 4)]}
        return {}

    monkeypatch.setattr(client, "_get", fake_get)
    return client


def test_fetch_customer_history_returns_ticket_summaries(mock_zd):
    from triage_cli.models import TicketSummary
    results = mock_zd.fetch_customer_history("ops@acme.com", limit=10)
    assert len(results) == 3
    assert all(isinstance(r, TicketSummary) for r in results)
    assert results[0].subject == "Ticket 1"


def test_fetch_customer_history_respects_limit(mock_zd, monkeypatch):
    from triage_cli.zendesk import ZendeskClient

    client = ZendeskClient.__new__(ZendeskClient)

    def fake_get(path, *, params=None, **kwargs):
        assert params is not None
        assert "per_page" in params or "page[size]" in params
        return {"results": []}

    monkeypatch.setattr(client, "_get", fake_get)
    client.fetch_customer_history("x@y.com", limit=5)


def test_fetch_customer_history_returns_empty_on_error(monkeypatch):
    from triage_cli.zendesk import ZendeskClient

    client = ZendeskClient.__new__(ZendeskClient)

    def fake_get(path, *, params=None, **kwargs):
        raise RuntimeError("network error")

    monkeypatch.setattr(client, "_get", fake_get)
    results = client.fetch_customer_history("ops@acme.com")
    assert results == []


def test_fetch_customer_history_empty_email_returns_empty(monkeypatch):
    from triage_cli.zendesk import ZendeskClient
    client = ZendeskClient.__new__(ZendeskClient)
    results = client.fetch_customer_history("")
    assert results == []
