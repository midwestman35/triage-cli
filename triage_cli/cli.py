"""Typer CLI for triage-cli: wires zendesk, extract, pipeline, and render together."""
from __future__ import annotations

import contextlib
import logging
import subprocess
import sys
from datetime import datetime
from pathlib import Path
from typing import NoReturn

import typer
from dotenv import load_dotenv

from triage_cli import extract, pipeline, render
from triage_cli.datadog import DatadogClient
from triage_cli.models import SiteEntry
from triage_cli.pipeline import spinner as _spinner
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
        _die(f"Invalid log levels: {invalid}. Valid: {sorted(_VALID_LEVELS)}")
    return parts


def _parse_backfill(value: str) -> float:
    """Parse the --backfill flag into hours. Accepts: inf, 0, Nh, Nd."""
    s = value.strip().lower()
    if s == "inf":
        return float("inf")
    if s == "0":
        return 0.0
    if s.endswith("h") and s[:-1].isdigit():
        return float(int(s[:-1]))
    if s.endswith("d") and s[:-1].isdigit():
        return float(int(s[:-1]) * 24)
    _die(f"--backfill must be 'inf', '0', 'Nh', or 'Nd' (got {value!r})")


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
    levels: str = typer.Option(
        "error,warn", "--levels", help="Datadog log levels: comma-separated"
    ),
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
        with ZendeskClient.from_env() as zd, _spinner(
            f"Fetching ticket #{ticket_id}", show=True
        ):
            ticket_obj = zd.get_ticket(ticket_id)
    except RuntimeError as e:
        _die(str(e))
    _vecho(verbose, f"Fetched ticket #{ticket_obj.id} — subject: {ticket_obj.subject}")

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

    # [6] Run pipeline (anchor + Datadog + LLM). DatadogClient lifetime spans the call.
    try:
        with contextlib.ExitStack() as stack:
            dd_client: DatadogClient | None = None
            if not no_logs:
                dd_client = stack.enter_context(DatadogClient.from_env())
            report = pipeline.triage_one(
                ticket_obj,
                site_entry,
                dd_client=dd_client,
                window_minutes=window_minutes,
                levels=level_list,
                at=at_dt,
                verbose=verbose,
                show_spinner=True,
            )
    except (RuntimeError, ValueError) as e:
        _die(str(e))

    # [7] Render.
    render.print_note(report)
    if save:
        md_path, json_path = render.save_note(report, ticket_obj.id)
        typer.echo(f"Saved: {md_path} and {json_path}", err=True)

    if verbose:
        sources_str = ", ".join(report.sources)
        typer.echo(
            f"Confidence: {report.confidence} · "
            f"events: {report.log_event_count} · "
            f"sources: {sources_str}",
            err=True,
        )


@app.command()
def watch(
    view: int = typer.Option(..., "--view", help="Zendesk view ID to watch"),
    interval: int = typer.Option(
        300, "--interval", min=10, help="Seconds to sleep after each iteration"
    ),
    state_file: Path | None = typer.Option(  # noqa: B008
        None,
        "--state-file",
        help="State file path (default: data/watcher-state-<view>.json)",
    ),
    backfill: str = typer.Option(
        "24h", "--backfill", help="Initial backfill horizon: inf, 0, Nh, Nd"
    ),
    window_minutes: int = typer.Option(
        30, "--window-minutes", min=1, help="Window radius around the anchor in minutes"
    ),
    levels: str = typer.Option(
        "error,warn", "--levels", help="Datadog log levels: comma-separated"
    ),
    no_logs: bool = typer.Option(
        False, "--no-logs", help="Skip Datadog; ticket-content-only triage"
    ),
    print_notes: bool = typer.Option(
        False, "--print-notes", help="Also print full markdown to stdout"
    ),
    verbose: bool = typer.Option(False, "--verbose", "-v"),
) -> None:
    """Poll a Zendesk view and triage new or updated tickets in a loop."""
    from triage_cli.watcher import WatcherOptions, run_watch

    logging.basicConfig(
        level=logging.INFO if verbose else logging.WARNING,
        format="%(levelname)s %(name)s: %(message)s",
    )

    level_list = _parse_levels(levels)
    backfill_hours = _parse_backfill(backfill)
    resolved_state = (
        state_file
        if state_file is not None
        else Path(f"data/watcher-state-{view}.json")
    )

    opts = WatcherOptions(
        view_id=view,
        interval=interval,
        state_file=resolved_state,
        backfill_hours=backfill_hours,
        window_minutes=window_minutes,
        levels=level_list,
        no_logs=no_logs,
        print_notes=print_notes,
        verbose=verbose,
    )
    try:
        run_watch(opts)
    # FileNotFoundError: missing site map; ValueError: corrupt state file.
    except (RuntimeError, FileNotFoundError, ValueError) as e:
        _die(str(e))


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
