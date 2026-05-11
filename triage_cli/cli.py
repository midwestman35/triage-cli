"""Typer CLI for triage-cli: wires zendesk, extract, pipeline, and render together."""
from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import os
import subprocess
import sys
from datetime import datetime
from pathlib import Path
from typing import NoReturn

import typer
from dotenv import load_dotenv

from triage_cli import extract, pipeline, render
from triage_cli.datadog import DatadogClient, DatadogError
from triage_cli.models import SiteEntry, TriageReport
from triage_cli.pipeline import spinner as _spinner
from triage_cli.zendesk import ZendeskClient

# Load .env at module import so every subcommand sees the same environment.
load_dotenv()

_VALID_LEVELS = {"error", "warn", "info", "debug"}
_SITE_MAP_PATH = Path("data/cnc-map.json")
_VIEWS_PATH = Path("data/views.json")

app = typer.Typer(no_args_is_help=True, add_completion=False)


def _die(msg: str) -> NoReturn:
    """Print a red error to stderr and exit with status 1."""
    typer.secho(f"Error: {msg}", fg=typer.colors.RED, err=True)
    raise typer.Exit(code=1)


def _vecho(verbose: bool, msg: str) -> None:
    """Echo to stderr only when --verbose is set, so stdout stays clean for piping."""
    if verbose:
        typer.echo(msg, err=True)


def _is_interactive() -> bool:
    """Return True when both stdin and stdout are connected to a real terminal."""
    return sys.stdin.isatty() and sys.stdout.isatty()


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


def _parse_paste(value: str) -> tuple[str, str]:
    """Parse a repeatable --paste LABEL=TEXT value."""
    label, sep, text = value.partition("=")
    if not sep or not label.strip():
        _die("--paste must be LABEL=TEXT")
    return label.strip(), text


def _resolve_view(view: str | None) -> tuple[int | None, str]:
    """Resolve --view to (view_id, state_key).

    None view means "my assigned tickets". A numeric string is used as-is.
    A non-numeric string is looked up in data/views.json.
    Returns (None, "me") for the personal queue.
    """
    if view is None:
        return None, "me"
    try:
        return int(view), view
    except ValueError:
        pass
    if _VIEWS_PATH.exists():
        try:
            named: dict = json.loads(_VIEWS_PATH.read_text())
            if view in named:
                return int(named[view]), view
        except (json.JSONDecodeError, ValueError):
            pass
    _die(f"Unknown view {view!r}. Use a numeric ID or a name from {_VIEWS_PATH}.")


def _configure_inbox_logging(view_key: str, verbose: bool) -> Path:
    """Route triage_cli logs to the inbox log file before Textual starts."""
    log_path = Path("data") / f"inbox-{view_key}.log"
    log_path.parent.mkdir(parents=True, exist_ok=True)

    log_logger = logging.getLogger("triage_cli")
    for handler in log_logger.handlers:
        handler.close()
    log_logger.handlers.clear()

    level = logging.DEBUG if verbose else logging.WARNING
    file_handler = logging.FileHandler(log_path, mode="a", encoding="utf-8")
    file_handler.setLevel(level)
    file_handler.setFormatter(
        logging.Formatter("%(asctime)s %(levelname)s %(name)s: %(message)s")
    )
    log_logger.addHandler(file_handler)
    log_logger.setLevel(level)
    log_logger.propagate = False

    return log_path


def _probe_zendesk() -> bool:
    """Return True when Zendesk env vars are present (no live HTTP call)."""
    required = ("ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN")
    return all(os.environ.get(v) for v in required)


@app.command()
def doctor() -> None:
    """Check env vars, credentials, and output-dir writability."""
    ok = True

    # Zendesk — critical
    typer.echo("Zendesk:", err=True)
    for var in ("ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN"):
        if os.environ.get(var):
            typer.echo(f"  ✓ {var}", err=True)
        else:
            typer.secho(f"  ✗ {var} not set", fg=typer.colors.RED, err=True)
            ok = False

    # LLM provider — critical
    provider = os.environ.get("LLM_PROVIDER", "unleash").lower()
    typer.echo(f"LLM provider: {provider}", err=True)
    provider_key_map = {
        "unleash": "UNLEASH_API_KEY",
        "claude": "ANTHROPIC_API_KEY",
        "openai": "OPENAI_API_KEY",
    }
    key_var = provider_key_map.get(provider)
    if key_var:
        if os.environ.get(key_var):
            typer.echo(f"  ✓ {key_var}", err=True)
        else:
            typer.secho(f"  ✗ {key_var} not set (required for LLM_PROVIDER={provider})", fg=typer.colors.RED, err=True)
            ok = False

    # Output dir — critical
    notes_dir = Path("triage-notes")
    try:
        notes_dir.mkdir(parents=True, exist_ok=True)
        probe = notes_dir / ".doctor-probe"
        probe.touch()
        probe.unlink()
        typer.echo("  ✓ triage-notes/ writable", err=True)
    except OSError as e:
        typer.secho(f"  ✗ triage-notes/ not writable: {e}", fg=typer.colors.RED, err=True)
        ok = False

    # Datadog — warn only
    dd_ok = os.environ.get("DD_API_KEY") and os.environ.get("DD_APP_KEY")
    if dd_ok:
        typer.echo("  ✓ Datadog configured (DD_API_KEY, DD_APP_KEY)", err=True)
    else:
        typer.secho("  ⚠ Datadog not configured — --no-logs will be forced", fg=typer.colors.YELLOW, err=True)

    if not ok:
        raise typer.Exit(code=1)


@app.command()
def investigate(
    ticket: str = typer.Argument(..., help="Zendesk ticket ID or full URL"),
    files: list[Path] = typer.Option(  # noqa: B008
        [],
        "--file",
        help="Pre-supplied local evidence file; may be repeated.",
        exists=False,
        file_okay=True,
        dir_okay=False,
        readable=True,
        resolve_path=True,
    ),
    pastes: list[str] = typer.Option(  # noqa: B008
        [],
        "--paste",
        help="Pre-supplied pasted evidence as LABEL=TEXT; may be repeated.",
    ),
    save: bool = typer.Option(
        True, "--save/--no-save",
        help="Save markdown/JSON to triage-notes/<id>/. Default: save.",
    ),
    no_llm: bool = typer.Option(
        False, "--no-llm",
        help="Skip the LLM call; produce the deterministic report instead.",
    ),
    no_logs: bool = typer.Option(
        False, "--no-logs", help="Skip Datadog; use ticket+evidence only.",
    ),
    window_minutes: int = typer.Option(30, "--window-minutes", min=1),
    at: str | None = typer.Option(None, "--at", help="Anchor override (ISO 8601)"),
    cnc: str | None = typer.Option(None, "--cnc", help="CNC UUID override"),
    site: str | None = typer.Option(None, "--site", help="site_name override"),
    levels: str = typer.Option(
        "error,warn", "--levels", help="Datadog log levels: comma-separated",
    ),
    verbose: bool = typer.Option(False, "--verbose", "-v"),
) -> None:
    """Run an interactive investigation on a Zendesk ticket."""
    if not _is_interactive():
        _die(
            "investigate requires an interactive terminal. "
            "Use 'triage' for headless runs."
        )

    try:
        ticket_id = extract.parse_ticket_id(ticket)
    except ValueError as e:
        _die(str(e))

    parsed_pastes = [_parse_paste(value) for value in pastes]
    at_dt: datetime | None = _parse_at(at) if at is not None else None
    level_list = _parse_levels(levels)

    for path in files:
        if not path.exists():
            _die(f"Local evidence file not found: {path}")
        if not path.is_file():
            _die(f"Local evidence path is not a file: {path}")

    try:
        with ZendeskClient.from_env() as zd:
            ticket_obj = zd.get_ticket(ticket_id)
    except RuntimeError as e:
        _die(str(e))

    _vecho(verbose, f"Fetched ticket #{ticket_obj.id} — subject: {ticket_obj.subject}")

    # Build the workspace before any prompts (downloads land in it).
    from triage_cli.interactive import (
        download_attachments,
        ensure_workspace,
        prompt_drop_and_wait,
        summarize_workspace,
    )
    workspace = ensure_workspace(Path("./triage-notes"), ticket_id=ticket_obj.id)

    # Stderr ticket header.
    typer.echo(
        f"ZD-{ticket_obj.id} · {ticket_obj.requester_org or '(no org)'} · "
        f"{sum(len(c.attachments) for c in ticket_obj.comments)} attachment(s) · "
        f"{len(ticket_obj.comments)} comment(s)",
        err=True,
    )

    # Step 4: attachment download prompt.
    try:
        with ZendeskClient.from_env() as zd:
            downloaded = download_attachments(ticket_obj, zd, workspace)
    except RuntimeError as e:
        _die(str(e))

    # Step 5: drop-and-ready loop.
    local_files_evidence = prompt_drop_and_wait(workspace)

    from triage_cli.investigation import (
        add_local_file as _add_local,
        add_pasted_evidence as _add_pasted,
        create_session as _create_session,
    )

    # Print workspace summary before the long pipeline run.
    typer.echo(
        summarize_workspace(
            workspace, local_files=local_files_evidence, downloaded=downloaded,
        ),
        err=True,
    )

    # Build session with all gathered evidence.
    session = _create_session(ticket_obj)
    for lf in local_files_evidence:
        session.evidence.local_files.append(lf)
    for d in downloaded:
        session.evidence.attachments.append(d)
    for path in files:
        try:
            _add_local(session, path)
        except OSError as e:
            _die(f"Could not read --file {path}: {e}")
    for label, text in parsed_pastes:
        _add_pasted(session, label, text)

    dd_client: DatadogClient | None = None
    if not no_logs:
        try:
            dd_client = DatadogClient.from_env()
        except Exception:
            _vecho(verbose, "Datadog not configured — skipping logs")

    try:
        report = asyncio.run(
            pipeline.investigate_one(
                ticket_obj,
                session=session,
                dd_client=dd_client,
                reporter=pipeline.StderrReporter(verbose=verbose),
                interactive=True,
                workspace=workspace.root,
                cnc_override=cnc,
                site_override=site,
                anchor_override=at_dt,
                window_minutes=window_minutes,
                levels=level_list,
                verbose=verbose,
                no_llm=no_llm,
            )
        )
    except (RuntimeError, ValueError) as e:
        _die(str(e))
    finally:
        if dd_client is not None:
            with contextlib.suppress(Exception):
                dd_client.close()

    render.print_note(report)


@app.command()
def triage(
    ticket: str = typer.Argument(..., help="Zendesk ticket ID or full URL"),
    verbose: bool = typer.Option(False, "--verbose", "-v"),
    no_logs: bool = typer.Option(False, "--no-logs", help="Skip Datadog; use ticket content only"),
    no_llm: bool = typer.Option(False, "--no-llm", help="Skip LLM; produce deterministic report"),
    window_minutes: int = typer.Option(30, "--window-minutes", min=1),
    at: str | None = typer.Option(None, "--at", help="Anchor timestamp override (ISO 8601)"),
    cnc: str | None = typer.Option(None, "--cnc", help="CNC UUID override"),
    site: str | None = typer.Option(None, "--site", help="site_name override (bypasses lookup)"),
    levels: str = typer.Option(
        "error,warn", "--levels", help="Datadog log levels: comma-separated"
    ),
) -> None:
    """Triage a single Zendesk ticket end-to-end (headless)."""
    logging.basicConfig(
        level=logging.INFO if verbose else logging.WARNING,
        format="%(levelname)s %(name)s: %(message)s",
    )

    try:
        ticket_id = extract.parse_ticket_id(ticket)
    except ValueError as e:
        _die(str(e))

    at_dt: datetime | None = _parse_at(at) if at is not None else None
    level_list = _parse_levels(levels)

    try:
        with ZendeskClient.from_env() as zd, _spinner(f"Fetching ticket #{ticket_id}", show=True):
            ticket_obj = zd.get_ticket(ticket_id)
    except RuntimeError as e:
        _die(str(e))
    _vecho(verbose, f"Fetched ticket #{ticket_obj.id} — subject: {ticket_obj.subject}")

    from triage_cli.investigation import create_session
    session = create_session(ticket_obj)

    dd_client: DatadogClient | None = None
    if not no_logs:
        try:
            dd_client = DatadogClient.from_env()
        except Exception:
            _vecho(verbose, "Datadog not configured — skipping logs")

    try:
        report = asyncio.run(
            pipeline.investigate_one(
                ticket_obj,
                session=session,
                dd_client=dd_client,
                reporter=pipeline.StderrReporter(verbose=verbose),
                interactive=False,
                cnc_override=cnc,
                site_override=site,
                anchor_override=at_dt,
                window_minutes=window_minutes,
                levels=level_list,
                verbose=verbose,
                no_llm=no_llm,
            )
        )
    except (RuntimeError, ValueError) as e:
        _die(str(e))
    finally:
        if dd_client is not None:
            with contextlib.suppress(Exception):
                dd_client.close()

    render.print_note(report)

    if verbose:
        sources_str = ", ".join(report.sources)
        typer.echo(
            f"Confidence: {report.confidence} · "
            f"events: {report.log_event_count} · "
            f"sources: {sources_str}",
            err=True,
        )


@app.command()
def inbox(
    view: str | None = typer.Option(
        None,
        "--view",
        help="View ID or named queue (e.g. 'unassigned'). Defaults to your assigned tickets.",
    ),
    poll: int = typer.Option(60, "--poll", min=10, help="Seconds between polls"),
    backfill: str = typer.Option(
        "0", "--backfill", help="Initial backfill horizon: inf, 0, Nh, Nd"
    ),
    window_minutes: int = typer.Option(
        15, "--window-minutes", min=1, help="Window radius around the anchor in minutes"
    ),
    levels: str = typer.Option(
        "error,warn", "--levels", help="Datadog log levels: comma-separated"
    ),
    no_logs: bool = typer.Option(False, "--no-logs", help="Skip Datadog; use ticket content only"),
    verbose: bool = typer.Option(False, "--verbose", "-v"),
) -> None:
    """Launch the interactive inbox TUI. Defaults to your assigned tickets."""
    if not _is_interactive():
        _die("inbox requires an interactive terminal. Use `watch` for headless runs.")

    from triage_cli.inbox import InboxApp
    from triage_cli.watcher import WatcherOptions

    view_id, view_key = _resolve_view(view)
    backfill_hours = _parse_backfill(backfill)
    level_list = _parse_levels(levels)
    state_file = Path("data") / f"watcher-state-{view_key}.json"
    state_file.parent.mkdir(parents=True, exist_ok=True)

    log_path = _configure_inbox_logging(view_key, verbose)

    opts = WatcherOptions(
        view_id=view_id,
        interval=poll,
        state_file=state_file,
        backfill_hours=backfill_hours,
        window_minutes=window_minutes,
        levels=level_list,
        no_logs=no_logs,
        print_notes=False,
        verbose=verbose,
    )
    typer.echo(f"Logging to {log_path}", err=True)
    InboxApp(opts).run()


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
