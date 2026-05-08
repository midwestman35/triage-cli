"""Focused tests for CLI-only behavior."""
from __future__ import annotations

import logging
from datetime import UTC, datetime
from pathlib import Path

from typer.testing import CliRunner

from triage_cli import cli
from triage_cli.models import Comment, Ticket


def _ticket(*, comments: list[Comment] | None = None) -> Ticket:
    created = datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    return Ticket(
        id=12345,
        subject="Audio drops on dispatch console",
        description="Caller audio drops after console reboot.",
        created_at=created,
        updated_at=datetime(2026, 5, 7, 14, 30, 0, tzinfo=UTC),
        comments=comments
        if comments is not None
        else [
            Comment(
                author="Agent One",
                body="Customer reports intermittent audio loss.",
                created_at=created,
                is_public=False,
            ),
        ],
    )


class _FakeZendeskClient:
    def __init__(self, ticket: Ticket) -> None:
        self.ticket = ticket
        self.fetched_ids: list[int] = []

    def __enter__(self) -> _FakeZendeskClient:
        return self

    def __exit__(self, *exc_info: object) -> None:
        return None

    def get_ticket(self, ticket_id: int) -> Ticket:
        self.fetched_ids.append(ticket_id)
        return self.ticket


def test_investigate_fetches_ticket_and_renders_report_without_enrichment(
    monkeypatch,
) -> None:
    ticket = _ticket()
    client = _FakeZendeskClient(ticket)
    touched: list[str] = []

    def forbidden(name: str):
        def _inner(*args: object, **kwargs: object) -> None:
            raise AssertionError(f"{name} should not be called")

        return _inner

    monkeypatch.setattr(cli.ZendeskClient, "from_env", lambda: client)
    monkeypatch.setattr(cli.extract, "load_site_map", forbidden("load_site_map"))
    monkeypatch.setattr(cli.pipeline, "resolve_site", forbidden("resolve_site"))
    monkeypatch.setattr(cli.pipeline, "triage_one", forbidden("triage_one"))
    monkeypatch.setattr(cli.DatadogClient, "from_env", forbidden("DatadogClient.from_env"))

    result = CliRunner().invoke(
        cli.app,
        ["investigate", "https://example.zendesk.com/agent/tickets/12345", "--verbose"],
    )

    assert result.exit_code == 0
    assert client.fetched_ids == [12345]
    assert "# Triage Report — ZD-12345" in result.stdout
    assert "**Site:** unknown" in result.stdout
    assert "**Sources:** zendesk, comments" in result.stdout
    assert "Fetched ticket #12345" in result.stderr
    assert "comments: 1" in result.stderr
    assert "attachments metadata: 0" in result.stderr
    assert "local files: 0" in result.stderr
    assert "pasted evidence: 0" in result.stderr
    assert "sources: zendesk, comments" in result.stderr
    assert touched == []


def test_investigate_adds_file_paste_and_saves_artifacts(
    tmp_path: Path,
    monkeypatch,
) -> None:
    log_path = tmp_path / "console.log"
    log_path.write_text("2026-05-07T14:10:00Z WARN audio dropped\n", encoding="utf-8")
    ticket = _ticket(comments=[])
    client = _FakeZendeskClient(ticket)

    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(cli.ZendeskClient, "from_env", lambda: client)

    result = CliRunner().invoke(
        cli.app,
        [
            "investigate",
            "12345",
            "--file",
            str(log_path),
            "--paste",
            "console=WARN audio",
            "--save",
            "--verbose",
        ],
    )

    assert result.exit_code == 0
    assert "**Sources:** zendesk, local_files, pasted_logs" in result.stdout
    assert "local files: 1" in result.stderr
    assert "pasted evidence: 1" in result.stderr
    assert "Saved:" in result.stderr
    saved_md = list((tmp_path / "triage-notes").glob("12345-*.md"))
    saved_json = list((tmp_path / "triage-notes").glob("12345-*.json"))
    assert len(saved_md) == 1
    assert len(saved_json) == 1
    assert "# Triage Report — ZD-12345" in saved_md[0].read_text(encoding="utf-8")


def test_investigate_rejects_malformed_paste() -> None:
    result = CliRunner().invoke(cli.app, ["investigate", "12345", "--paste", "WARN audio"])

    assert result.exit_code == 1
    assert "--paste must be LABEL=TEXT" in result.stderr


def test_investigate_rejects_missing_local_file(tmp_path: Path, monkeypatch) -> None:
    monkeypatch.setattr(cli.ZendeskClient, "from_env", lambda: _FakeZendeskClient(_ticket()))

    result = CliRunner().invoke(
        cli.app,
        ["investigate", "12345", "--file", str(tmp_path / "missing.log")],
    )

    assert result.exit_code == 1
    assert "Local evidence file not found" in result.stderr


def test_configure_inbox_logging_replaces_handlers_and_writes_file(
    tmp_path,
    monkeypatch,
) -> None:
    monkeypatch.chdir(tmp_path)
    logger = logging.getLogger("triage_cli")
    saved_handlers = logger.handlers[:]
    saved_level = logger.level
    saved_propagate = logger.propagate
    logger.handlers.clear()
    stale_handler = logging.StreamHandler()
    logger.addHandler(stale_handler)

    try:
        log_path = cli._configure_inbox_logging(view_key="42", verbose=True)
        assert log_path == tmp_path.joinpath("data", "inbox-42.log").relative_to(
            tmp_path
        )
        assert stale_handler not in logger.handlers
        assert logger.level == logging.DEBUG
        assert logger.propagate is False

        [handler] = logger.handlers
        assert isinstance(handler, logging.FileHandler)
        assert handler.level == logging.DEBUG

        logging.getLogger("triage_cli.inbox").debug("poll started")
        handler.flush()
        assert "DEBUG triage_cli.inbox: poll started" in (tmp_path / log_path).read_text(
            encoding="utf-8"
        )
    finally:
        for handler in logger.handlers:
            handler.close()
        logger.handlers[:] = saved_handlers
        logger.setLevel(saved_level)
        logger.propagate = saved_propagate


def test_inbox_requires_interactive_terminal() -> None:
    result = CliRunner().invoke(cli.app, ["inbox", "--view", "42"])

    assert result.exit_code == 1
    assert (
        "inbox requires an interactive terminal. Use `watch` for headless runs."
        in result.stderr
    )
