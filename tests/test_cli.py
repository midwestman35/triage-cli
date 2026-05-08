"""Focused tests for CLI-only behavior."""
from __future__ import annotations

import logging

from typer.testing import CliRunner

from triage_cli import cli


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
