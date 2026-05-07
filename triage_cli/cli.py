"""Typer CLI for triage-cli: wires zendesk, extract, datadog, llm, and render together."""
from __future__ import annotations

import asyncio
import logging
import subprocess
import sys
from datetime import datetime
from pathlib import Path
from typing import NoReturn

import typer
from dotenv import load_dotenv

from triage_cli import extract, render
from triage_cli.datadog import DatadogClient
from triage_cli.llm import extract_anchor as llm_extract_anchor
from triage_cli.llm import triage as llm_triage
from triage_cli.models import SiteEntry, TriageBundle
from triage_cli.zendesk import ZendeskClient

# Load .env at module import so every subcommand sees the same environment.
load_dotenv()

_VALID_LEVELS = {"error", "warn", "info", "debug"}
_SITE_MAP_PATH = Path("data/cnc-map.json")

app = typer.Typer(no_args_is_help=True, add_completion=False)


def _die(msg: str) -> NoReturn:
    """Print a red error to stderr and exit with status 1."""
    typer.secho(f"Error: {msg}", fg=typer.colors.RED, err=True)
    raise typer.Exit(code=1)


def _vecho(verbose: bool, msg: str) -> None:
    """Echo to stderr only when --verbose is set, so stdout stays clean for piping."""
    if verbose:
        typer.echo(msg, err=True)


def _parse_at(at: str) -> datetime:
    """Parse an ISO 8601 anchor override; accept trailing Z."""
    try:
        return datetime.fromisoformat(at.replace("Z", "+00:00"))
    except ValueError as e:
        _die(f"--at must be ISO 8601 (got {at!r}): {e}")


def _parse_levels(levels: str) -> list[str]:
    """Split, lowercase, and validate the --levels flag."""
    parts = [s.strip().lower() for s in levels.split(",") if s.strip()]
    if not parts:
        _die("--levels must be a non-empty comma-separated list")
    invalid = [p for p in parts if p not in _VALID_LEVELS]
    if invalid:
        _die(
            f"Invalid log levels: {invalid}. Valid: {sorted(_VALID_LEVELS)}"
        )
    return parts


@app.command()
def triage(
    ticket: str = typer.Argument(..., help="Zendesk ticket ID or full URL"),
    save: bool = typer.Option(False, "--save", help="Also save the note to ./triage-notes/"),
    verbose: bool = typer.Option(False, "--verbose", "-v"),
    no_logs: bool = typer.Option(False, "--no-logs", help="Skip Datadog; use ticket content only"),
    window_minutes: int = typer.Option(30, "--window-minutes", min=1),
    at: str | None = typer.Option(None, "--at", help="Anchor timestamp override (ISO 8601)"),
    cnc: str | None = typer.Option(None, "--cnc", help="CNC UUID override"),
    site: str | None = typer.Option(None, "--site", help="site_name override (bypasses lookup)"),
    levels: str = typer.Option("error,warn", "--levels", help="Datadog log levels: comma-separated"),
    no_interactive: bool = typer.Option(
        False,
        "--no-interactive",
        help="Abort instead of prompting if site can't be resolved",
    ),
) -> None:
    """Triage a single Zendesk ticket end-to-end."""
    logging.basicConfig(
        level=logging.INFO if verbose else logging.WARNING,
        format="%(levelname)s %(name)s: %(message)s",
    )

    # [1] Parse ticket id.
    try:
        ticket_id = extract.parse_ticket_id(ticket)
    except ValueError as e:
        _die(str(e))

    # [2] Validate flags.
    at_dt: datetime | None = _parse_at(at) if at is not None else None
    level_list = _parse_levels(levels)

    # [3] Fetch ticket.
    try:
        with ZendeskClient.from_env() as zd:
            ticket_obj = zd.get_ticket(ticket_id)
    except RuntimeError as e:
        _die(str(e))
    _vecho(
        verbose,
        f"Fetched ticket #{ticket_obj.id} — subject: {ticket_obj.subject}",
    )

    # [4] Load site map.
    try:
        sites = extract.load_site_map(_SITE_MAP_PATH)
    except (FileNotFoundError, ValueError) as e:
        _die(f"{e}\nHint: run 'triage-cli build-map' to (re)generate {_SITE_MAP_PATH}.")

    # [5] Resolve site.
    try:
        site_entry, strategy = extract.lookup_site(
            ticket_obj, sites, cnc_override=cnc, site_override=site,
        )
    except ValueError as e:
        _die(str(e))

    if site_entry is None:
        if no_interactive:
            _die(
                "could not resolve site for ticket; use --site or --cnc, "
                "or remove --no-interactive"
            )
        manual = typer.prompt("Enter site_name to query").strip()
        if not manual:
            _die("site_name cannot be empty")
        site_entry = SiteEntry(friendly_name="(manual)", site_name=manual, cnc="")
        strategy = "interactive_prompt"
    _vecho(
        verbose,
        f"Site resolved via {strategy}: {site_entry.site_name} ({site_entry.friendly_name})",
    )

    # [6] Resolve anchor.
    extracted_dt: datetime | None = None
    if not no_logs and at_dt is None:
        # Let SDK errors surface; user can pass --at or --no-logs to skip extraction.
        extracted_dt = asyncio.run(llm_extract_anchor(ticket_obj))
    anchor_dt, anchor_source = extract.resolve_anchor(
        ticket_obj, at_flag=at_dt, extracted=extracted_dt,
    )
    _vecho(verbose, f"Anchor: {anchor_dt.isoformat()} from {anchor_source.value}")

    # [7] Fetch logs (skip if --no-logs). Build the window either way for the bundle.
    start, end = extract.build_window(anchor_dt, window_minutes)
    log_lines: list = []
    log_truncated = False
    if not no_logs:
        try:
            with DatadogClient.from_env() as dd:
                log_lines, log_truncated = dd.get_logs(
                    site_entry.site_name, level_list, start, end,
                )
        except (RuntimeError, ValueError) as e:
            _die(str(e))
        _vecho(
            verbose,
            f"Pulled {len(log_lines)} log lines (truncated={log_truncated})",
        )

    # [8] Build bundle.
    bundle = TriageBundle(
        ticket=ticket_obj,
        site_entry=site_entry,
        log_lines=log_lines,
        log_truncated=log_truncated,
        anchor=anchor_dt,
        anchor_source=anchor_source,
        window_start=start,
        window_end=end,
    )

    # [9] Call LLM and render.
    markdown = asyncio.run(llm_triage(bundle))
    render.print_note(markdown)
    if save:
        path = render.save_note(markdown, ticket_obj.id)
        typer.echo(f"\nSaved to: {path}", err=True)


@app.command("build-map")
def build_map() -> None:
    """Rebuild data/cnc-map.json and data/cnc-map-gaps.md from apex-cnc-inventory.md."""
    repo_root = Path(__file__).resolve().parent.parent
    script = repo_root / "scripts" / "build_cnc_map.py"
    if not script.exists():
        _die(f"build_cnc_map.py not found at {script}")
    result = subprocess.run([sys.executable, str(script)], cwd=repo_root)
    raise typer.Exit(result.returncode)


if __name__ == "__main__":  # pragma: no cover
    app()
