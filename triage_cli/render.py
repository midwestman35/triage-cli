"""Render triage notes to stdout and optionally to disk."""

from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path

import typer

DEFAULT_OUTPUT_DIR = Path("./triage-notes")


def print_note(markdown: str) -> None:
    """Print the rendered triage note to stdout via typer.echo."""
    typer.echo(markdown)


def save_note(
    markdown: str,
    ticket_id: int,
    output_dir: Path | None = None,
) -> Path:
    """Write the note to <output_dir>/<ticket_id>-<utc-timestamp>.md and return the path."""
    target_dir = output_dir if output_dir is not None else DEFAULT_OUTPUT_DIR
    target_dir.mkdir(parents=True, exist_ok=True)

    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    filename = f"{ticket_id}-{timestamp}.md"
    path = target_dir / filename

    content = markdown if markdown.endswith("\n") else markdown + "\n"
    path.write_text(content, encoding="utf-8")
    return path
