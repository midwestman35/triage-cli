"""Typer CLI for triage-cli: wires zendesk, extract, pipeline, and render together."""
from __future__ import annotations

import contextlib
import json
import logging
import subprocess
import sys
from datetime import datetime
from pathlib import Path
from typing import NoReturn

import typer
from dotenv import load_dotenv

from triage_cli import evidence, extract, investigation, pipeline, render
from triage_cli.datadog import DatadogClient, DatadogError
from triage_cli.investigation import InvestigationSession
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

    # [5] Resolve site (substring match → LLM fallback → interactive prompt).
    try:
        site_entry, strategy = pipeline.resolve_site(
            ticket_obj, sites,
            cnc_override=cnc, site_override=site,
            verbose=verbose, show_spinner=True,
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
    def _run_pipeline(use_dd: bool) -> TriageReport:
        with contextlib.ExitStack() as stack:
            dd_client: DatadogClient | None = None
            if use_dd:
                dd_client = stack.enter_context(DatadogClient.from_env())
            return pipeline.triage_one(
                ticket_obj,
                site_entry,
                dd_client=dd_client,
                window_minutes=window_minutes,
                levels=level_list,
                at=at_dt,
                verbose=verbose,
                show_spinner=True,
            )

    try:
        report = _run_pipeline(use_dd=not no_logs)
    except DatadogError as e:
        typer.echo(f"Datadog error: {e}", err=True)
        if no_interactive or not sys.stderr.isatty():
            _die("Aborting — use --no-logs to skip Datadog")
        if not typer.confirm("Continue without Datadog logs?", default=False):
            _die("Aborted")
        try:
            report = _run_pipeline(use_dd=False)
        except (RuntimeError, ValueError) as e2:
            _die(str(e2))
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
    verbose: bool = typer.Option(False, "--verbose", "-v"),
) -> None:
    """Launch the interactive inbox TUI. Defaults to your assigned tickets."""
    if not sys.stdout.isatty() or not sys.stdin.isatty():
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
        no_logs=False,
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


def _summary_lines(ticket_obj, attachments: list[dict]) -> list[str]:
    """Render a short stderr summary for the start of an investigation."""
    public = sum(1 for c in ticket_obj.comments if c.is_public)
    internal = len(ticket_obj.comments) - public
    org = ticket_obj.requester_org or "(unset)"
    created = ticket_obj.created_at.strftime("%Y-%m-%d %H:%M UTC")
    lines = [
        f"Ticket #{ticket_obj.id}",
        f"  Subject:     {ticket_obj.subject}",
        f"  Requester:   {org}",
        f"  Created:     {created}",
        f"  Comments:    {len(ticket_obj.comments)} ({public} public, {internal} internal)",
    ]
    if attachments:
        names = ", ".join(
            str(a.get("file_name") or "(unnamed)") for a in attachments[:5]
        )
        suffix = "" if len(attachments) <= 5 else f", +{len(attachments) - 5} more"
        lines.append(f"  Attachments: {len(attachments)} ({names}{suffix})")
    else:
        lines.append("  Attachments: 0")
    return lines


def _read_paste_block() -> str:
    """Read multi-line text from stdin until EOF or a single '.' on a line."""
    typer.echo(
        "  Paste your text. End with a single '.' on its own line, or Ctrl-D.",
        err=True,
    )
    chunks: list[str] = []
    while True:
        line = sys.stdin.readline()
        if not line:
            break
        if line.strip() == ".":
            break
        chunks.append(line)
    return "".join(chunks)


def _evidence_menu(session: InvestigationSession, *, attachments_pending: bool) -> str:
    """Render the evidence-source menu and return the user's normalized choice."""
    typer.echo("", err=True)
    typer.echo(f"Sources so far: {len(session.sources)} · Timeline: {len(session.timeline)} events",
               err=True)
    typer.echo("Add evidence:", err=True)
    if attachments_pending:
        typer.echo("  [a] Ingest Zendesk attachment metadata", err=True)
    typer.echo("  [f] Add local file", err=True)
    typer.echo("  [d] Add local directory", err=True)
    typer.echo("  [p] Paste log text", err=True)
    typer.echo("  [s] Proceed to assessment", err=True)
    raw = typer.prompt("Choice", default="s").strip().lower()
    if not raw:
        return "s"
    return raw[0]


@app.command()
def investigate(
    ticket: str = typer.Argument(..., help="Zendesk ticket ID or full URL"),
    save: bool = typer.Option(
        True, "--save/--no-save", help="Save markdown + JSON to ./triage-notes/"
    ),
    verbose: bool = typer.Option(False, "--verbose", "-v"),
) -> None:
    """Guided investigation: fetch a ticket, ingest evidence, produce a triage note."""
    logging.basicConfig(
        level=logging.INFO if verbose else logging.WARNING,
        format="%(levelname)s %(name)s: %(message)s",
    )

    if not sys.stdin.isatty():
        _die("investigate requires an interactive terminal. Use `triage` for headless runs.")

    try:
        ticket_id = extract.parse_ticket_id(ticket)
    except ValueError as e:
        _die(str(e))

    try:
        with ZendeskClient.from_env() as zd:
            with _spinner(f"Fetching ticket #{ticket_id}", show=True):
                ticket_obj = zd.get_ticket(ticket_id)
            with _spinner("Listing attachments", show=True):
                attachments = zd.list_attachments(ticket_id)
    except RuntimeError as e:
        _die(str(e))

    session = InvestigationSession(ticket=ticket_obj)
    src, evs = evidence.from_ticket(ticket_obj)
    investigation.add_source(session, src, evs)
    src, evs = evidence.from_comments(ticket_obj)
    investigation.add_source(session, src, evs)

    typer.echo("", err=True)
    for line in _summary_lines(ticket_obj, attachments):
        typer.echo(line, err=True)

    attachments_pending = bool(attachments)
    while True:
        choice = _evidence_menu(session, attachments_pending=attachments_pending)
        if choice == "s":
            break
        if choice == "a" and attachments_pending:
            sources = evidence.attachments_metadata(attachments, ticket_id=ticket_obj.id)
            for att_src in sources:
                investigation.add_source(session, att_src, [])
            typer.echo(f"  recorded {len(sources)} attachment(s) as metadata-only", err=True)
            attachments_pending = False
            continue
        if choice == "f":
            path_str = typer.prompt("File path").strip()
            try:
                src, evs = evidence.from_local_file(Path(path_str).expanduser())
            except (FileNotFoundError, OSError) as e:
                typer.echo(f"  error: {e}", err=True)
                continue
            investigation.add_source(session, src, evs)
            note = src.notes or "all parsed"
            typer.echo(f"  added {src.label}: {src.event_count} events ({note})", err=True)
            continue
        if choice == "d":
            path_str = typer.prompt("Directory path").strip()
            pattern = typer.prompt("Glob pattern", default="*.log").strip()
            try:
                src, evs = evidence.from_local_directory(
                    Path(path_str).expanduser(), pattern=pattern
                )
            except (FileNotFoundError, NotADirectoryError, OSError) as e:
                typer.echo(f"  error: {e}", err=True)
                continue
            investigation.add_source(session, src, evs)
            typer.echo(f"  added {src.label}: {src.event_count} events ({src.notes})", err=True)
            continue
        if choice == "p":
            label = typer.prompt("Label for this paste", default="paste").strip() or "paste"
            text = _read_paste_block()
            if not text.strip():
                typer.echo("  (empty paste skipped)", err=True)
                continue
            src, evs = evidence.from_pasted_text(text, label=label)
            investigation.add_source(session, src, evs)
            note = src.notes or "all parsed"
            typer.echo(f"  added {src.label}: {src.event_count} events ({note})", err=True)
            continue
        typer.echo(f"  unknown choice: {choice!r}", err=True)

    typer.echo("", err=True)
    typer.echo(f"Running assessment over {len(session.sources)} source(s), "
               f"{len(session.timeline)} event(s)…", err=True)
    try:
        with _spinner("Generating triage note", show=True):
            report = investigation.run_assessment(session, verbose=verbose)
    except (RuntimeError, ValueError) as e:
        _die(str(e))

    render.print_note(report)
    if save:
        md_path, json_path = render.save_note(report, ticket_obj.id)
        typer.echo(f"Saved: {md_path} and {json_path}", err=True)

    if verbose:
        typer.echo(
            f"Confidence: {report.confidence} · sources: {len(session.sources)} · "
            f"events: {report.log_event_count}",
            err=True,
        )


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
