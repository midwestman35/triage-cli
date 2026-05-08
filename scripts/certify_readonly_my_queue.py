"""Read-only certification runner for the authenticated Zendesk assigned queue."""
from __future__ import annotations

import argparse
import os
import sys
from collections.abc import Sequence
from pathlib import Path
from typing import NoReturn

from dotenv import load_dotenv

from triage_cli import render
from triage_cli.investigation import (
    add_local_file,
    add_pasted_evidence,
    build_timeline,
    create_session,
    session_to_report,
)
from triage_cli.zendesk import ZendeskClient

REQUIRED_ZENDESK_ENV = ("ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN")


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Certify Guided Investigation against only the authenticated user's "
            "assigned Zendesk queue."
        )
    )
    parser.add_argument(
        "--ticket-id",
        type=int,
        help="Assigned ticket ID to certify. Defaults to the first assigned queue ticket.",
    )
    parser.add_argument(
        "--file",
        action="append",
        default=[],
        type=Path,
        dest="files",
        help="Local evidence file to include; may be repeated.",
    )
    parser.add_argument(
        "--paste",
        action="append",
        default=[],
        dest="pastes",
        help="Pasted evidence as LABEL=TEXT; may be repeated.",
    )
    return parser


def _status(message: str) -> None:
    print(message, file=sys.stderr)


def _error(message: str) -> int:
    print(f"Error: {message}", file=sys.stderr)
    return 1


def _env_presence() -> list[str]:
    missing: list[str] = []
    for name in REQUIRED_ZENDESK_ENV:
        if os.environ.get(name):
            _status(f"{name}: set")
        else:
            _status(f"{name}: missing")
            missing.append(name)
    return missing


def _validate_files(paths: Sequence[Path]) -> bool:
    for path in paths:
        if not path.exists():
            _error(f"Local evidence file not found: {path}")
            return False
        if not path.is_file():
            _error(f"Local evidence path is not a file: {path}")
            return False
        try:
            with path.open("rb") as handle:
                handle.read(1)
        except OSError as exc:
            _error(f"Could not read local evidence file {path}: {exc}")
            return False
    return True


def _parse_paste(value: str) -> tuple[str, str]:
    label, sep, text = value.partition("=")
    if not sep or not label.strip():
        raise ValueError("--paste must be LABEL=TEXT")
    return label.strip(), text


def _parse_pastes(values: Sequence[str]) -> list[tuple[str, str]] | None:
    parsed: list[tuple[str, str]] = []
    for value in values:
        try:
            parsed.append(_parse_paste(value))
        except ValueError as exc:
            _error(str(exc))
            return None
    return parsed


def _select_ticket_id(
    assigned_ticket_ids: list[int],
    requested_ticket_id: int | None,
) -> int | None:
    if not assigned_ticket_ids:
        _error("Authenticated user's assigned queue is empty; cannot certify.")
        return None

    if requested_ticket_id is None:
        return assigned_ticket_ids[0]

    if requested_ticket_id not in assigned_ticket_ids:
        _error(
            f"Ticket ID {requested_ticket_id} is not in the authenticated user's assigned queue."
        )
        return None
    return requested_ticket_id


def main(argv: Sequence[str] | None = None, *, load_env: bool = True) -> int:
    if load_env:
        load_dotenv()
    args = _parser().parse_args(argv)

    missing = _env_presence()
    if missing:
        return _error(
            "Missing required Zendesk environment variables: " + ", ".join(missing)
        )

    if not _validate_files(args.files):
        return 1
    parsed_pastes = _parse_pastes(args.pastes)
    if parsed_pastes is None:
        return 1

    try:
        with ZendeskClient.from_env() as zendesk:
            assigned_ticket_ids = zendesk.list_my_ticket_ids()
            _status(f"assigned queue count: {len(assigned_ticket_ids)}")

            ticket_id = _select_ticket_id(assigned_ticket_ids, args.ticket_id)
            if ticket_id is None:
                return 1
            _status(f"selected ticket ID: {ticket_id}")

            ticket = zendesk.get_ticket(ticket_id)
    except RuntimeError as exc:
        return _error(str(exc))

    _status(f"Fetched ticket #{ticket.id} - subject: {ticket.subject}")

    session = create_session(ticket)
    for path in args.files:
        try:
            add_local_file(session, path)
        except OSError as exc:
            return _error(f"Could not read local evidence file {path}: {exc}")
    for label, text in parsed_pastes:
        add_pasted_evidence(session, label, text)

    build_timeline(session)
    report = session_to_report(session)
    _status(
        "Investigation evidence: "
        f"comments: {len(session.evidence.comments)} - "
        f"attachments metadata: {len(session.evidence.attachments)} - "
        f"local files: {len(session.evidence.local_files)} - "
        f"pasted evidence: {len(session.evidence.pasted_logs)} - "
        f"sources: {', '.join(report.sources)}"
    )
    print(render.to_markdown(report))
    return 0


def _main() -> NoReturn:
    raise SystemExit(main())


if __name__ == "__main__":
    _main()
