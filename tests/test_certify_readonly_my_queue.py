"""Tests for the read-only assigned-queue certification runner."""
from __future__ import annotations

import importlib
import subprocess
import sys
from datetime import UTC, datetime
from pathlib import Path

import pytest

from triage_cli.models import AttachmentEvidence, Comment, Ticket

REQUIRED_ENV = ("ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN")


def _load_script():
    sys.modules.pop("scripts.certify_readonly_my_queue", None)
    return importlib.import_module("scripts.certify_readonly_my_queue")


def _set_env(monkeypatch: pytest.MonkeyPatch) -> None:
    for name in REQUIRED_ENV:
        monkeypatch.setenv(name, f"dummy-{name.lower()}")


def _ticket(ticket_id: int = 123) -> Ticket:
    created = datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    return Ticket(
        id=ticket_id,
        subject="Audio drops on dispatch console",
        description="Caller audio drops after console reboot.",
        requester_org=None,
        tags=[],
        created_at=created,
        updated_at=datetime(2026, 5, 7, 14, 30, 0, tzinfo=UTC),
        comments=[
            Comment(
                author="Agent One",
                body="Customer reports intermittent audio loss.",
                created_at=datetime(2026, 5, 7, 14, 5, 0, tzinfo=UTC),
                is_public=False,
                attachments=[
                    AttachmentEvidence(
                        filename="station_logs.zip",
                        content_type="application/zip",
                        size_bytes=4096,
                    ),
                ],
            ),
        ],
    )


class FakeZendeskClient:
    calls: list[tuple[str, int | None]] = []
    ticket_ids: list[int] = [123, 456]
    ticket: Ticket = _ticket()
    instantiated: bool = False

    def __init__(self) -> None:
        type(self).instantiated = True

    @classmethod
    def from_env(cls) -> FakeZendeskClient:
        return cls()

    def __enter__(self) -> FakeZendeskClient:
        return self

    def __exit__(self, *exc_info: object) -> None:
        return None

    def list_my_ticket_ids(self) -> list[int]:
        type(self).calls.append(("list_my_ticket_ids", None))
        return list(type(self).ticket_ids)

    def get_ticket(self, ticket_id: int) -> Ticket:
        type(self).calls.append(("get_ticket", ticket_id))
        return type(self).ticket.model_copy(update={"id": ticket_id})


@pytest.fixture(autouse=True)
def reset_fake_client() -> None:
    FakeZendeskClient.calls = []
    FakeZendeskClient.ticket_ids = [123, 456]
    FakeZendeskClient.ticket = _ticket()
    FakeZendeskClient.instantiated = False


def test_missing_env_exits_nonzero_and_does_not_instantiate_zendesk(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    script = _load_script()
    for name in REQUIRED_ENV:
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setattr(script, "ZendeskClient", FakeZendeskClient)

    result = script.main([], load_env=False)

    captured = capsys.readouterr()
    assert result != 0
    assert not FakeZendeskClient.instantiated
    assert "ZENDESK_SUBDOMAIN: missing" in captured.err
    assert "ZENDESK_EMAIL: missing" in captured.err
    assert "ZENDESK_API_TOKEN: missing" in captured.err
    assert "Missing required Zendesk environment variables" in captured.err
    assert captured.out == ""


def test_success_uses_assigned_queue_first_ticket_and_prints_status(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    script = _load_script()
    _set_env(monkeypatch)
    monkeypatch.setattr(script, "ZendeskClient", FakeZendeskClient)

    result = script.main([])

    captured = capsys.readouterr()
    assert result == 0
    assert FakeZendeskClient.calls == [("list_my_ticket_ids", None), ("get_ticket", 123)]
    assert "# Triage Report" in captured.out
    assert "ZD-123" in captured.out
    assert "Customer reports intermittent audio loss" in captured.out
    assert "ZENDESK_SUBDOMAIN: set" in captured.err
    assert "assigned queue count: 2" in captured.err
    assert "selected ticket ID: 123" in captured.err
    assert "Fetched ticket #123" in captured.err
    assert "comments: 1" in captured.err
    assert "attachments metadata: 1" in captured.err
    assert "sources: zendesk, comments" in captured.err


def test_importing_runner_does_not_import_datadog_or_pipeline() -> None:
    code = """
import importlib
import sys

importlib.import_module("scripts.certify_readonly_my_queue")
forbidden = [
    name for name in ("triage_cli.datadog", "triage_cli.pipeline") if name in sys.modules
]
if forbidden:
    print(",".join(forbidden), file=sys.stderr)
    raise SystemExit(1)
"""

    result = subprocess.run(
        [sys.executable, "-c", code],
        cwd=Path(__file__).resolve().parents[1],
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr


def test_explicit_ticket_id_in_assigned_queue_is_fetched(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    script = _load_script()
    _set_env(monkeypatch)
    monkeypatch.setattr(script, "ZendeskClient", FakeZendeskClient)

    result = script.main(["--ticket-id", "456"])

    captured = capsys.readouterr()
    assert result == 0
    assert FakeZendeskClient.calls == [("list_my_ticket_ids", None), ("get_ticket", 456)]
    assert "ZD-456" in captured.out
    assert "selected ticket ID: 456" in captured.err


def test_empty_assigned_queue_exits_before_ticket_fetch(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    script = _load_script()
    _set_env(monkeypatch)
    FakeZendeskClient.ticket_ids = []
    monkeypatch.setattr(script, "ZendeskClient", FakeZendeskClient)

    result = script.main([])

    captured = capsys.readouterr()
    assert result != 0
    assert FakeZendeskClient.calls == [("list_my_ticket_ids", None)]
    assert "assigned queue count: 0" in captured.err
    assert "assigned queue is empty" in captured.err
    assert captured.out == ""


def test_ticket_id_not_in_assigned_queue_is_rejected_before_fetch(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    script = _load_script()
    _set_env(monkeypatch)
    monkeypatch.setattr(script, "ZendeskClient", FakeZendeskClient)

    result = script.main(["--ticket-id", "999"])

    captured = capsys.readouterr()
    assert result != 0
    assert FakeZendeskClient.calls == [("list_my_ticket_ids", None)]
    assert "Ticket ID 999 is not in the authenticated user's assigned queue" in captured.err
    assert captured.out == ""


def test_optional_file_and_paste_evidence_are_reflected_without_saving(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    script = _load_script()
    _set_env(monkeypatch)
    evidence_path = tmp_path / "station.log"
    evidence_path.write_text("2026-05-07T14:10:00Z WARN audio dropped\n", encoding="utf-8")
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(script, "ZendeskClient", FakeZendeskClient)

    result = script.main(
        [
            "--file",
            str(evidence_path),
            "--paste",
            "console=Operator observed silence after reboot",
        ]
    )

    captured = capsys.readouterr()
    assert result == 0
    assert "WARN audio dropped" in captured.out
    assert "Operator observed silence after reboot" in captured.out
    assert "local files: 1" in captured.err
    assert "pasted evidence: 1" in captured.err
    assert "sources: zendesk, comments, attachments, local_files, pasted_logs" in captured.err
    assert not (tmp_path / "triage-notes").exists()


def test_rejects_bad_optional_evidence_before_zendesk_fetch(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    script = _load_script()
    _set_env(monkeypatch)
    monkeypatch.setattr(script, "ZendeskClient", FakeZendeskClient)

    missing_result = script.main(["--file", str(tmp_path / "missing.log")])
    malformed_result = script.main(["--paste", "not label equals text"])

    captured = capsys.readouterr()
    assert missing_result != 0
    assert malformed_result != 0
    assert FakeZendeskClient.calls == []
    assert "Local evidence file not found" in captured.err
    assert "--paste must be LABEL=TEXT" in captured.err


def test_certify_records_only_get_calls(monkeypatch: pytest.MonkeyPatch) -> None:
    """_record_request appends the normalised method name to _calls."""
    import httpx

    from scripts import certify_readonly_my_queue as cert

    cert._calls.clear()  # type: ignore[attr-defined]

    # Patch the real request so no actual network call is made.
    monkeypatch.setattr(
        cert,
        "_original_request",
        lambda self, method, url, **kw: httpx.Response(200),
    )

    cert._record_request(httpx.Client(), "GET", "http://x")  # type: ignore[attr-defined]
    assert cert._calls == ["GET"]  # type: ignore[attr-defined]


def test_certify_assertion_flags_non_get_call(capsys: pytest.CaptureFixture[str]) -> None:
    """_assert_only_get_calls returns 2 and prints to stderr when non-GET seen."""
    from scripts import certify_readonly_my_queue as cert

    cert._calls.clear()  # type: ignore[attr-defined]
    cert._calls.extend(["GET", "POST", "GET"])  # type: ignore[attr-defined]

    rc = cert._assert_only_get_calls()  # type: ignore[attr-defined]
    assert rc == 2
    captured = capsys.readouterr()
    assert "POST" in captured.err
    assert "FAIL" in captured.err


def test_certify_assertion_passes_for_only_gets() -> None:
    """_assert_only_get_calls returns 0 for all-GET history."""
    from scripts import certify_readonly_my_queue as cert

    cert._calls.clear()  # type: ignore[attr-defined]
    cert._calls.extend(["GET", "GET", "GET"])  # type: ignore[attr-defined]

    rc = cert._assert_only_get_calls()  # type: ignore[attr-defined]
    assert rc == 0


def test_rejects_unreadable_optional_evidence_before_zendesk_fetch(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
) -> None:
    script = _load_script()
    _set_env(monkeypatch)
    evidence_path = tmp_path / "unreadable.log"
    evidence_path.write_text("cannot read this during preflight\n", encoding="utf-8")
    original_open = Path.open

    def fail_for_evidence_file(self: Path, *args: object, **kwargs: object) -> object:
        if self == evidence_path:
            raise OSError("simulated read failure")
        return original_open(self, *args, **kwargs)

    monkeypatch.setattr(Path, "open", fail_for_evidence_file)
    monkeypatch.setattr(script, "ZendeskClient", FakeZendeskClient)

    result = script.main(["--file", str(evidence_path)])

    captured = capsys.readouterr()
    assert result != 0
    assert not FakeZendeskClient.instantiated
    assert FakeZendeskClient.calls == []
    assert "Could not read local evidence file" in captured.err
    assert "simulated read failure" in captured.err
    assert captured.out == ""
