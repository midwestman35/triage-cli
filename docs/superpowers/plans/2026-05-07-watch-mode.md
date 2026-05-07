# Watch Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `triage-cli watch --view <id>` that polls a Zendesk view and runs the existing single-ticket triage pipeline against new or updated tickets, saving notes to disk and emitting a structured per-ticket status stream to stderr.

**Architecture:** Extract the post-resolution part of `cli.triage` into `triage_cli/pipeline.py::triage_one(ticket, site_entry, ...)`. Add a new `triage_cli/watcher.py` that owns state I/O, a pure `should_triage` decider, and a sequential `run_iteration` orchestrator. `cli.watch` is a thin Typer command that loops `run_iteration` with a sleep-after-iteration cadence.

**Tech Stack:** Python 3.11+, Pydantic 2, Typer, httpx, pytest, claude-agent-sdk, datadog-api-client, unicode-animations.

**Spec:** `docs/superpowers/specs/2026-05-07-watch-mode-design.md`

---

## File Structure

**New files:**
- `triage_cli/pipeline.py` — `triage_one(ticket, site_entry, ...) -> str`. Owns spinner; imports `llm`, `extract`, `datadog`, `models`. No site resolution, no rendering.
- `triage_cli/watcher.py` — `WatcherOptions` dataclass, state I/O, `should_triage`, `prune_state`, `run_iteration`, `run_watch`.
- `tests/test_zendesk.py` — pagination test for the new `list_view_ticket_ids` method.
- `tests/test_pipeline.py` — one integration-style test for `triage_one` with stubbed LLM and Datadog.
- `tests/test_watcher.py` — state, decider, prune, iteration orchestration.
- `docs/runbooks/06-watching-a-view.md` — operator runbook.

**Modified files:**
- `triage_cli/models.py` — add `Ticket.updated_at: datetime`.
- `triage_cli/zendesk.py` — parse `updated_at` in `get_ticket`; add `list_view_ticket_ids`.
- `triage_cli/cli.py` — restructure `triage` (drop dead `ImportError` guard, dedupe verbose, call `pipeline.triage_one`); add `watch` command; move `_spinner` to `pipeline.py` and import it back.
- `tests/test_extract.py` — `_ticket()` helper defaults `updated_at` to `created_at`.
- `.gitignore` — add `data/watcher-state-*.json`.
- `README.md` — new "Watching a Zendesk view" section; add `pipeline.py` and `watcher.py` to project layout.

**Decision: where does `_spinner` live?** It moves to `pipeline.py` (the heaviest user — 3 of 4 spinner contexts) and `cli.py` imports it back for the ticket-fetch spinner. Single source of truth, no circular import (cli imports pipeline; pipeline does not import cli).

**Decision: how does `pipeline.triage_one` handle Datadog?** It accepts an optional `dd_client: DatadogClient | None` parameter. `None` means `--no-logs`. The caller (cli.triage or watcher.run_iteration) owns the lifetime via `contextlib.ExitStack`. This lets watch mode reuse one `DatadogClient` across all tickets in an iteration.

---

## Task 1: Add `Ticket.updated_at`

**Files:**
- Modify: `triage_cli/models.py:42-51` (Ticket class)
- Modify: `triage_cli/zendesk.py:90-98` (get_ticket return)
- Modify: `tests/test_extract.py:20-36` (_ticket helper)

The watch-mode fingerprint is `Ticket.updated_at`. The Zendesk API returns it on `/tickets/<id>.json` already; we just need to parse it. Existing tests build `Ticket` instances via a helper that doesn't set `updated_at`, so the helper gets one default-line change.

- [ ] **Step 1: Add `updated_at` field to the Ticket model**

Edit `triage_cli/models.py`. Locate the `Ticket` class (currently at line 42). Add `updated_at: datetime` immediately after `created_at`:

```python
class Ticket(BaseModel):
    """A Zendesk ticket with its full chronological comment thread."""

    id: int
    subject: str
    description: str
    requester_org: str | None = None
    tags: list[str] = Field(default_factory=list)
    created_at: datetime
    updated_at: datetime
    comments: list[Comment] = Field(default_factory=list)
```

- [ ] **Step 2: Run the test suite — expect failures in test_extract.py**

Run: `pytest tests/ -q`
Expected: ~32 failures of the form `pydantic.ValidationError: ... updated_at: Field required`. This confirms the field is required and existing fixtures are missing it.

- [ ] **Step 3: Update the `_ticket` helper to default `updated_at` to `created_at`**

Edit `tests/test_extract.py`. Find the `_ticket` helper (lines 20-36) and add `updated_at`:

```python
def _ticket(
    *,
    subject: str = "",
    description: str = "",
    requester_org: str | None = None,
    created_at: datetime | None = None,
    updated_at: datetime | None = None,
) -> Ticket:
    created = created_at or datetime(2026, 5, 1, 12, 0, 0, tzinfo=timezone.utc)
    return Ticket(
        id=1,
        subject=subject,
        description=description,
        requester_org=requester_org,
        tags=[],
        created_at=created,
        updated_at=updated_at or created,
        comments=[],
    )
```

- [ ] **Step 4: Run the suite — expect green**

Run: `pytest tests/ -q`
Expected: 32 passed.

- [ ] **Step 5: Parse `updated_at` in ZendeskClient.get_ticket**

Edit `triage_cli/zendesk.py`. Find the `get_ticket` method's return statement (currently line 90-98). Add `updated_at`:

```python
        return Ticket(
            id=int(ticket_obj["id"]),
            subject=ticket_obj.get("subject") or "",
            description=ticket_obj.get("description") or "",
            requester_org=requester_org,
            tags=list(ticket_obj.get("tags") or []),
            created_at=_parse_iso(ticket_obj["created_at"]),
            updated_at=_parse_iso(ticket_obj["updated_at"]),
            comments=self._fetch_comments(ticket_id),
        )
```

- [ ] **Step 6: Run the suite again — still green**

Run: `pytest tests/ -q`
Expected: 32 passed.

- [ ] **Step 7: Commit**

```bash
git add triage_cli/models.py triage_cli/zendesk.py tests/test_extract.py
git commit -m "$(cat <<'EOF'
feat(models): add Ticket.updated_at for watch-mode fingerprinting

Zendesk returns updated_at on every ticket payload; parse it alongside
created_at and surface it on the Ticket model. Existing test helper
defaults updated_at to created_at so all current tests stay green.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Extract `pipeline.triage_one` (with spinner cleanup)

**Files:**
- Create: `triage_cli/pipeline.py`
- Create: `tests/test_pipeline.py`
- Modify: `triage_cli/cli.py:1-216` (the entire `triage` command and module-level `_spinner`)

This is the keystone refactor. Behavior must not change for `cli.triage` users. The spinner cleanup (dead `ImportError` guard, dedupe verbose) is absorbed because every spinner/`_vecho` call moves anyway.

The new boundary: `pipeline.triage_one(ticket, site_entry, *, dd_client, window_minutes, levels, at, verbose, show_spinner) -> str`.

- [ ] **Step 1: Write the failing test for `pipeline.triage_one`**

Create `tests/test_pipeline.py`:

```python
"""Tests for triage_cli.pipeline.triage_one (orchestration only)."""
from __future__ import annotations

from datetime import datetime, timezone

import pytest

from triage_cli import pipeline
from triage_cli.models import SiteEntry, Ticket


def _ticket() -> Ticket:
    ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    return Ticket(
        id=42,
        subject="audio dropouts on console",
        description="see logs",
        requester_org="Aurora 911, CO",
        tags=[],
        created_at=ts,
        updated_at=ts,
        comments=[],
    )


def _site() -> SiteEntry:
    return SiteEntry(
        friendly_name="Aurora 911, CO",
        site_name="us-co-aurora-apex",
        cnc="921d7c53-e815-4566-9692-6cbce589e1d3",
    )


def test_triage_one_no_logs_path(monkeypatch: pytest.MonkeyPatch) -> None:
    """With dd_client=None, pipeline skips Datadog and returns the LLM markdown."""
    expected = "## Summary\nstub triage note\n"

    async def fake_triage(_bundle, model=None):  # noqa: ARG001
        return expected

    async def fake_extract_anchor(_ticket, model=None):  # noqa: ARG001
        return None

    monkeypatch.setattr(pipeline, "_llm_triage", fake_triage)
    monkeypatch.setattr(pipeline, "_llm_extract_anchor", fake_extract_anchor)

    result = pipeline.triage_one(
        _ticket(),
        _site(),
        dd_client=None,
        window_minutes=30,
        levels=["error", "warn"],
        at=None,
        verbose=False,
        show_spinner=False,
    )
    assert result == expected
```

- [ ] **Step 2: Run the test to confirm it fails**

Run: `pytest tests/test_pipeline.py -v`
Expected: FAIL with `ModuleNotFoundError: No module named 'triage_cli.pipeline'`.

- [ ] **Step 3: Create `triage_cli/pipeline.py` with `_spinner` and `triage_one`**

Create `triage_cli/pipeline.py`:

```python
"""End-to-end triage pipeline for a fetched ticket and a resolved site.

Owns the LLM and Datadog calls; does not handle ticket fetch, site
resolution, output rendering, or persistence. Two callers today:
`cli.triage` (with interactive site prompt) and `watcher.run_iteration`
(skips on no-match).
"""
from __future__ import annotations

import asyncio
import contextlib
import logging
import sys
from collections.abc import Iterator
from datetime import datetime
from typing import TYPE_CHECKING

import typer
from unicode_animations import live_spinner as _live_spinner

from triage_cli import extract
from triage_cli.llm import extract_anchor as _llm_extract_anchor
from triage_cli.llm import triage as _llm_triage
from triage_cli.models import SiteEntry, Ticket, TriageBundle

if TYPE_CHECKING:
    from triage_cli.datadog import DatadogClient

logger = logging.getLogger(__name__)


@contextlib.contextmanager
def spinner(text: str, *, show: bool) -> Iterator[None]:
    """Show an 'orbit' loading spinner during a slow op when stderr is a TTY."""
    if show and sys.stderr.isatty():
        with _live_spinner("orbit", text=text, stream=sys.stderr):
            yield
    else:
        yield


def _vecho(verbose: bool, msg: str) -> None:
    """Echo to stderr only when verbose is set."""
    if verbose:
        typer.echo(msg, err=True)


def triage_one(
    ticket: Ticket,
    site_entry: SiteEntry,
    *,
    dd_client: "DatadogClient | None",
    window_minutes: int,
    levels: list[str],
    at: datetime | None,
    verbose: bool,
    show_spinner: bool,
) -> str:
    """Run the triage pipeline for a fetched ticket and resolved site.

    Returns the rendered markdown.
    Raises RuntimeError on Datadog or Claude failure.
    Raises ValueError on validation failure (e.g. invalid window).
    """
    # Anchor: best-effort LLM extraction unless --at was supplied.
    extracted_dt: datetime | None = None
    if dd_client is not None and at is None:
        try:
            with spinner("Asking Claude to extract incident timestamp", show=show_spinner):
                extracted_dt = asyncio.run(_llm_extract_anchor(ticket))
        except Exception as e:
            _vecho(verbose, f"Anchor extraction via Claude failed: {e}; falling back to created_at")

    anchor_dt, anchor_source = extract.resolve_anchor(
        ticket, at_flag=at, extracted=extracted_dt,
    )
    _vecho(verbose, f"Anchor: {anchor_dt.isoformat()} from {anchor_source.value}")

    start, end = extract.build_window(anchor_dt, window_minutes)
    log_lines: list = []
    log_truncated = False
    if dd_client is None:
        _vecho(verbose, "Skipping Datadog (--no-logs)")
    else:
        with spinner(f"Querying Datadog for {site_entry.site_name}", show=show_spinner):
            log_lines, log_truncated = dd_client.get_logs(
                site_entry.site_name, levels, start, end,
            )
        _vecho(verbose, f"Pulled {len(log_lines)} log lines (truncated={log_truncated})")

    bundle = TriageBundle(
        ticket=ticket,
        site_entry=site_entry,
        log_lines=log_lines,
        log_truncated=log_truncated,
        anchor=anchor_dt,
        anchor_source=anchor_source,
        window_start=start,
        window_end=end,
    )

    with spinner("Generating triage note", show=show_spinner):
        markdown = asyncio.run(_llm_triage(bundle))
    return markdown
```

- [ ] **Step 4: Run the pipeline test — expect green**

Run: `pytest tests/test_pipeline.py -v`
Expected: 1 passed.

- [ ] **Step 5: Refactor `cli.py` to call `pipeline.triage_one`, drop the dead `ImportError` guard, dedupe verbose**

Open `triage_cli/cli.py` and replace its full contents with the version below. Read carefully — multiple changes are bundled:

```python
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
            markdown = pipeline.triage_one(
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
```

Note the changes vs. the previous `cli.py`:
- Dead `try/except ImportError` for `unicode_animations` is gone (it was unreachable; the package is a hard dependency).
- `_spinner` is now imported from `pipeline` and used as `_spinner(text, show=True)` (the new keyword-only `show` parameter replaces the old TTY-or-noop logic).
- The duplicate `_vecho("Querying Datadog for ... levels=... window=...min")` line is gone — the spinner shows the site name, and pipeline's own `_vecho` covers post-fetch summary.
- The async/orchestration block is replaced by a single `pipeline.triage_one(...)` call inside an `ExitStack` that owns the `DatadogClient` lifetime.

- [ ] **Step 6: Run the full suite — expect 33 passed**

Run: `pytest tests/ -q`
Expected: 33 passed (32 from test_extract.py + 1 from test_pipeline.py).

- [ ] **Step 7: Smoke-test the `triage` command help still works**

Run: `python -m triage_cli.cli --help` (or `triage-cli --help` if installed)
Expected: usage shows `triage` and `build-map` subcommands; no import errors.

- [ ] **Step 8: Commit**

```bash
git add triage_cli/pipeline.py triage_cli/cli.py tests/test_pipeline.py
git commit -m "$(cat <<'EOF'
refactor(cli): extract triage pipeline; drop dead spinner guard

Move the post-resolution part of cli.triage (anchor extraction,
Datadog fetch, LLM triage) into pipeline.triage_one so watch mode can
share it. Spinner moves with it; cli imports spinner back. Drop the
unreachable ImportError guard on unicode_animations (hard dependency)
and the duplicate "Querying Datadog for X" verbose line that
shadowed the spinner.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Add `ZendeskClient.list_view_ticket_ids`

**Files:**
- Modify: `triage_cli/zendesk.py` (add a method; reuse pagination pattern)
- Create: `tests/test_zendesk.py`

The view-list endpoint is `/views/{view_id}/tickets.json`. Same cursor-or-legacy pagination shape as `_fetch_comments`. Use the existing `_get` helper; catch its 404 RuntimeError and re-raise with a view-flavored message.

- [ ] **Step 1: Write the failing test**

Create `tests/test_zendesk.py`:

```python
"""Tests for triage_cli.zendesk.ZendeskClient."""
from __future__ import annotations

from typing import Any

import httpx
import pytest

from triage_cli.zendesk import ZendeskClient


def _client() -> ZendeskClient:
    return ZendeskClient(subdomain="example", email="e@x.com", api_token="tok")


def test_list_view_ticket_ids_paginates(monkeypatch: pytest.MonkeyPatch) -> None:
    """list_view_ticket_ids walks cursor pagination and returns IDs in order received."""
    pages: list[dict[str, Any]] = [
        {
            "tickets": [{"id": 1}, {"id": 2}],
            "meta": {"has_more": True},
            "links": {"next": "https://example.zendesk.com/api/v2/views/9/tickets.json?page=2"},
        },
        {
            "tickets": [{"id": 3}],
            "meta": {"has_more": False},
            "links": {},
        },
    ]
    page_iter = iter(pages)

    def fake_get(self: httpx.Client, url: str, params: Any = None) -> httpx.Response:  # noqa: ARG001
        return httpx.Response(200, json=next(page_iter))

    monkeypatch.setattr(httpx.Client, "get", fake_get)

    with _client() as zd:
        ids = zd.list_view_ticket_ids(9)

    assert ids == [1, 2, 3]


def test_list_view_ticket_ids_404_message(monkeypatch: pytest.MonkeyPatch) -> None:
    """A 404 from the views endpoint surfaces a view-flavored error message."""

    def fake_get(self: httpx.Client, url: str, params: Any = None) -> httpx.Response:  # noqa: ARG001
        return httpx.Response(404, json={"error": "not found"})

    monkeypatch.setattr(httpx.Client, "get", fake_get)

    with _client() as zd, pytest.raises(RuntimeError, match="View 999 not found"):
        zd.list_view_ticket_ids(999)
```

- [ ] **Step 2: Run the test — expect failure**

Run: `pytest tests/test_zendesk.py -v`
Expected: FAIL with `AttributeError: 'ZendeskClient' object has no attribute 'list_view_ticket_ids'`.

- [ ] **Step 3: Implement `list_view_ticket_ids`**

Edit `triage_cli/zendesk.py`. Add the method directly after `get_ticket` (so it sits before the private `_fetch_comments` and `_get` methods):

```python
    def list_view_ticket_ids(self, view_id: int) -> list[int]:
        """Return ticket IDs in the given Zendesk view, in the order returned.

        Paginates via cursor (meta.has_more + links.next) with legacy next_page
        fallback. Raises RuntimeError on transport failure or non-2xx status;
        a 404 surfaces a view-flavored message.
        """
        path: str | None = f"/views/{view_id}/tickets.json"
        params: dict[str, Any] | None = {"page[size]": _PAGE_SIZE}
        ids: list[int] = []

        for _ in range(_MAX_PAGES):
            if path is None:
                break
            try:
                payload = self._get(path, params=params, ticket_id=view_id)
            except RuntimeError as e:
                if "not found" in str(e).lower():
                    raise RuntimeError(f"View {view_id} not found") from e
                raise
            for t in payload.get("tickets") or []:
                if "id" in t:
                    ids.append(int(t["id"]))

            meta = payload.get("meta") or {}
            links = payload.get("links") or {}
            if meta.get("has_more") and links.get("next"):
                path = links["next"]
            else:
                path = payload.get("next_page")
            params = None
        else:
            raise RuntimeError(
                f"Zendesk view pagination exceeded {_MAX_PAGES} pages - possible loop"
            )
        return ids
```

- [ ] **Step 4: Run the tests — expect green**

Run: `pytest tests/test_zendesk.py -v`
Expected: 2 passed.

- [ ] **Step 5: Run the full suite to make sure nothing else broke**

Run: `pytest tests/ -q`
Expected: 35 passed.

- [ ] **Step 6: Commit**

```bash
git add triage_cli/zendesk.py tests/test_zendesk.py
git commit -m "$(cat <<'EOF'
feat(zendesk): add list_view_ticket_ids with cursor pagination

Paginate /views/{id}/tickets.json the same way _fetch_comments does
(cursor + legacy fallback, MAX_PAGES safety cap). 404s on the view
endpoint surface a view-flavored RuntimeError instead of the generic
"Ticket X not found" string from _get.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Add `triage_cli/watcher.py` (state, decider, prune)

**Files:**
- Create: `triage_cli/watcher.py`
- Create: `tests/test_watcher.py`

Build the watcher in pieces, smallest to largest: dataclass + state I/O → `should_triage` → `prune_state` → `run_iteration` → `run_watch`. Each piece commits separately to keep diffs focused.

This task covers the first three (state I/O, `should_triage`, `prune_state`). Tasks 5 covers `run_iteration` + `run_watch`. Task 6 wires `cli.watch`.

- [ ] **Step 1: Write the failing tests for `WatcherOptions`, state I/O, `load_state` defaults, version mismatch**

Create `tests/test_watcher.py`:

```python
"""Tests for triage_cli.watcher."""
from __future__ import annotations

from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from triage_cli.models import Ticket
from triage_cli.watcher import (
    WatcherOptions,
    load_state,
    prune_state,
    save_state,
    should_triage,
)


def _ticket(ticket_id: int, updated_at: datetime) -> Ticket:
    return Ticket(
        id=ticket_id,
        subject="x",
        description="x",
        requester_org=None,
        tags=[],
        created_at=updated_at,
        updated_at=updated_at,
        comments=[],
    )


def _opts(state_file: Path) -> WatcherOptions:
    return WatcherOptions(
        view_id=1,
        interval=300,
        state_file=state_file,
        backfill_hours=24.0,
        window_minutes=30,
        levels=["error", "warn"],
        no_logs=False,
        print_notes=False,
        verbose=False,
    )


def test_load_state_returns_empty_default_when_missing(tmp_path: Path) -> None:
    state = load_state(tmp_path / "missing.json")
    assert state == {"version": 1, "triaged": {}}


def test_load_state_round_trips_save_state(tmp_path: Path) -> None:
    path = tmp_path / "state.json"
    original = {"version": 1, "triaged": {"42": "2026-05-07T12:00:00+00:00"}}
    save_state(path, original)
    assert load_state(path) == original


def test_save_state_atomic_no_temp_left_behind(tmp_path: Path) -> None:
    path = tmp_path / "state.json"
    save_state(path, {"version": 1, "triaged": {}})
    assert path.exists()
    assert not (path.parent / (path.name + ".tmp")).exists()


def test_load_state_rejects_unknown_version(tmp_path: Path) -> None:
    path = tmp_path / "state.json"
    path.write_text('{"version": 99, "triaged": {}}', encoding="utf-8")
    with pytest.raises(RuntimeError, match="version 99"):
        load_state(path)
```

- [ ] **Step 2: Run the tests — expect failure (module missing)**

Run: `pytest tests/test_watcher.py -v`
Expected: FAIL with `ModuleNotFoundError: No module named 'triage_cli.watcher'`.

- [ ] **Step 3: Create `triage_cli/watcher.py` with `WatcherOptions`, `load_state`, `save_state`**

Create `triage_cli/watcher.py`:

```python
"""Long-running watcher: poll a Zendesk view and triage new/updated tickets.

Public surface:
    WatcherOptions       - frozen dataclass of run-time knobs
    load_state           - read a watcher state file (or default)
    save_state           - atomic write of a state dict
    should_triage        - pure decider: (ticket, state, cutoff) -> bool
    prune_state          - bounded-growth helper; keeps N most-recent entries
    run_iteration        - single poll-and-triage pass over a view
    run_watch            - the main loop; sleeps interval seconds between iterations
"""
from __future__ import annotations

import json
import logging
import math
import os
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from triage_cli.models import Ticket

logger = logging.getLogger(__name__)

STATE_VERSION = 1
DEFAULT_PRUNE_CAP = 1000


@dataclass(frozen=True)
class WatcherOptions:
    view_id: int
    interval: int
    state_file: Path
    backfill_hours: float
    window_minutes: int
    levels: list[str]
    no_logs: bool
    print_notes: bool
    verbose: bool


State = dict[str, Any]


def _empty_state() -> State:
    return {"version": STATE_VERSION, "triaged": {}}


def load_state(path: Path) -> State:
    """Load a watcher state file. Returns the empty default when missing.

    Raises RuntimeError on a version we don't understand.
    """
    if not path.exists():
        return _empty_state()
    raw = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(raw, dict) or "version" not in raw or "triaged" not in raw:
        raise RuntimeError(f"State file {path} is not a valid watcher state object")
    version = raw["version"]
    if version != STATE_VERSION:
        raise RuntimeError(
            f"State file {path} has version {version}; this watcher supports "
            f"version {STATE_VERSION}"
        )
    triaged = raw["triaged"]
    if not isinstance(triaged, dict):
        raise RuntimeError(f"State file {path} 'triaged' must be an object")
    return {"version": STATE_VERSION, "triaged": dict(triaged)}


def save_state(path: Path, state: State) -> None:
    """Atomically write the state file (tempfile + os.replace)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.parent / (path.name + ".tmp")
    tmp.write_text(json.dumps(state, indent=2, sort_keys=True), encoding="utf-8")
    os.replace(tmp, path)
```

- [ ] **Step 4: Run the state-IO tests — expect green**

Run: `pytest tests/test_watcher.py -v`
Expected: 4 passed.

- [ ] **Step 5: Add the `should_triage` failing tests**

Append to `tests/test_watcher.py`:

```python
def test_should_triage_true_when_absent_and_within_cutoff() -> None:
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    cutoff = now - timedelta(hours=24)
    state = {"version": 1, "triaged": {}}
    assert should_triage(_ticket(42, now), state, cutoff) is True


def test_should_triage_false_when_absent_but_older_than_cutoff() -> None:
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    cutoff = now - timedelta(hours=1)
    older = now - timedelta(hours=2)
    state = {"version": 1, "triaged": {}}
    assert should_triage(_ticket(42, older), state, cutoff) is False


def test_should_triage_false_when_state_matches_updated_at() -> None:
    when = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    cutoff = when - timedelta(hours=24)
    state = {"version": 1, "triaged": {"42": when.isoformat()}}
    assert should_triage(_ticket(42, when), state, cutoff) is False


def test_should_triage_true_when_ticket_newer_than_state() -> None:
    earlier = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    later = earlier + timedelta(minutes=5)
    cutoff = earlier - timedelta(hours=24)
    state = {"version": 1, "triaged": {"42": earlier.isoformat()}}
    assert should_triage(_ticket(42, later), state, cutoff) is True
```

- [ ] **Step 6: Run — expect 4 new failures (NameError on should_triage)**

Run: `pytest tests/test_watcher.py -v`
Expected: 4 passed, 4 failed (failures are `should_triage` tests).

- [ ] **Step 7: Implement `should_triage`**

Append to `triage_cli/watcher.py`:

```python
def should_triage(ticket: Ticket, state: State, backfill_cutoff: datetime) -> bool:
    """Decide whether to triage a ticket given current state and the cutoff.

    Rules:
      - If the ticket's updated_at is older than the cutoff and we have no
        record of it, return False (it'll be silently marked as seen by
        run_iteration).
      - If state has the same updated_at we already triaged this version.
      - If state has an older updated_at OR no entry at all (and we're past
        the cutoff check), triage.
    """
    triaged = state.get("triaged") or {}
    key = str(ticket.id)
    stored = triaged.get(key)
    if stored is None:
        return ticket.updated_at >= backfill_cutoff
    try:
        stored_dt = datetime.fromisoformat(stored)
    except ValueError:
        # Corrupt entry — re-triage to recover.
        logger.warning("watcher: corrupt timestamp for ticket %s in state: %r", key, stored)
        return True
    return ticket.updated_at > stored_dt
```

- [ ] **Step 8: Run — expect 8 passed**

Run: `pytest tests/test_watcher.py -v`
Expected: 8 passed.

- [ ] **Step 9: Add the `prune_state` failing test**

Append to `tests/test_watcher.py`:

```python
def test_prune_state_keeps_n_most_recent() -> None:
    triaged = {
        "1": "2026-05-01T12:00:00+00:00",
        "2": "2026-05-02T12:00:00+00:00",
        "3": "2026-05-03T12:00:00+00:00",
        "4": "2026-05-04T12:00:00+00:00",
    }
    state = {"version": 1, "triaged": triaged}
    pruned = prune_state(state, max_entries=2)
    assert set(pruned["triaged"].keys()) == {"3", "4"}
```

- [ ] **Step 10: Run — expect 1 failure (NameError)**

Run: `pytest tests/test_watcher.py::test_prune_state_keeps_n_most_recent -v`
Expected: FAIL with `NameError: name 'prune_state' is not defined`.

- [ ] **Step 11: Implement `prune_state`**

Append to `triage_cli/watcher.py`:

```python
def prune_state(state: State, max_entries: int = DEFAULT_PRUNE_CAP) -> State:
    """Keep at most max_entries triaged entries, dropping the oldest by timestamp."""
    triaged = state.get("triaged") or {}
    if len(triaged) <= max_entries:
        return {"version": STATE_VERSION, "triaged": dict(triaged)}
    items = sorted(triaged.items(), key=lambda kv: kv[1], reverse=True)
    kept = dict(items[:max_entries])
    return {"version": STATE_VERSION, "triaged": kept}
```

- [ ] **Step 12: Run all watcher tests — expect 9 passed**

Run: `pytest tests/test_watcher.py -v`
Expected: 9 passed.

- [ ] **Step 13: Run full suite**

Run: `pytest tests/ -q`
Expected: 44 passed (32 + 1 pipeline + 2 zendesk + 9 watcher).

- [ ] **Step 14: Commit**

```bash
git add triage_cli/watcher.py tests/test_watcher.py
git commit -m "$(cat <<'EOF'
feat(watcher): add WatcherOptions, state I/O, should_triage, prune_state

Foundation for watch mode: a frozen-dataclass options bag, atomic
state load/save with a version-1 schema, a pure should_triage
decider that compares ticket.updated_at against stored fingerprint
+ backfill cutoff, and a bounded-growth pruner that keeps the N
most-recent entries.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Add `run_iteration` and `run_watch`

**Files:**
- Modify: `triage_cli/watcher.py` (append `run_iteration`, `run_watch`, helpers)
- Modify: `tests/test_watcher.py` (add 2 tests)

`run_iteration` is the orchestrator. It:
1. Lists view ticket IDs.
2. Per ID: fetch ticket → decide via `should_triage`.
3. On `True`: resolve site (no prompt) → call `pipeline.triage_one` → save note → mark state.
4. On `False`-with-cutoff: silently mark state.
5. On `False`-with-match: print `unchanged`.
6. On per-ticket exception or no_match: print `skipped`/`failed`, do not mark state.
7. After loop: prune + save_state.

Tests inject a fake `ZendeskClient`, monkeypatch `pipeline.triage_one` and `extract.lookup_site` as needed.

- [ ] **Step 1: Write the failing tests for `run_iteration`**

Append to `tests/test_watcher.py`:

```python
from io import StringIO
from unittest.mock import MagicMock

from triage_cli.models import SiteEntry
from triage_cli.watcher import run_iteration


@pytest.fixture
def stub_sites() -> list[SiteEntry]:
    return [
        SiteEntry(
            friendly_name="Aurora 911, CO",
            site_name="us-co-aurora-apex",
            cnc="921d7c53-e815-4566-9692-6cbce589e1d3",
        ),
    ]


def _zd_with_tickets(tickets: dict[int, Ticket], view_ids: list[int]) -> MagicMock:
    zd = MagicMock()
    zd.list_view_ticket_ids.return_value = view_ids
    zd.get_ticket.side_effect = lambda tid: tickets[tid]
    return zd


def test_run_iteration_marks_only_successfully_triaged(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    stub_sites: list[SiteEntry],
) -> None:
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    cutoff = now - timedelta(hours=24)
    t_ok = Ticket(
        id=1, subject="s", description="us-co-aurora-apex", requester_org=None,
        tags=[], created_at=now, updated_at=now, comments=[],
    )
    t_fail = Ticket(
        id=2, subject="s", description="us-co-aurora-apex", requester_org=None,
        tags=[], created_at=now, updated_at=now, comments=[],
    )
    t_no_site = Ticket(
        id=3, subject="s", description="no match here", requester_org=None,
        tags=[], created_at=now, updated_at=now, comments=[],
    )
    zd = _zd_with_tickets({1: t_ok, 2: t_fail, 3: t_no_site}, [1, 2, 3])

    def fake_triage_one(ticket, site_entry, **kwargs):  # noqa: ARG001
        if ticket.id == 2:
            raise RuntimeError("simulated Datadog timeout")
        return f"## Summary\nnote for {ticket.id}\n"

    monkeypatch.setattr("triage_cli.pipeline.triage_one", fake_triage_one)
    monkeypatch.setattr("triage_cli.render.save_note", lambda md, tid: tmp_path / f"{tid}.md")

    state = {"version": 1, "triaged": {}}
    opts = _opts(tmp_path / "state.json")
    new_state = run_iteration(zd, stub_sites, state, opts, cutoff, dd_client=None)

    assert new_state["triaged"] == {"1": now.isoformat()}


def test_run_iteration_status_lines(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    capsys: pytest.CaptureFixture,
    stub_sites: list[SiteEntry],
) -> None:
    now = datetime(2026, 5, 7, 12, 0, 0, tzinfo=timezone.utc)
    cutoff = now - timedelta(hours=24)
    tickets = {
        1: Ticket(id=1, subject="s", description="us-co-aurora-apex", requester_org=None,
                  tags=[], created_at=now, updated_at=now, comments=[]),
        2: Ticket(id=2, subject="s", description="us-co-aurora-apex", requester_org=None,
                  tags=[], created_at=now, updated_at=now, comments=[]),
        3: Ticket(id=3, subject="s", description="no match", requester_org=None,
                  tags=[], created_at=now, updated_at=now, comments=[]),
        4: Ticket(id=4, subject="s", description="us-co-aurora-apex", requester_org=None,
                  tags=[], created_at=now, updated_at=now, comments=[]),
    }
    zd = _zd_with_tickets(tickets, [1, 2, 3, 4])

    def fake_triage_one(ticket, site_entry, **kwargs):  # noqa: ARG001
        if ticket.id == 2:
            raise RuntimeError("Datadog timeout")
        return f"note {ticket.id}"

    monkeypatch.setattr("triage_cli.pipeline.triage_one", fake_triage_one)
    monkeypatch.setattr("triage_cli.render.save_note", lambda md, tid: Path(f"/tmp/{tid}.md"))

    state = {"version": 1, "triaged": {"4": now.isoformat()}}
    opts = _opts(tmp_path / "state.json")
    run_iteration(zd, stub_sites, state, opts, cutoff, dd_client=None)
    err = capsys.readouterr().err

    assert "#1 triaged" in err
    assert "#2 failed" in err and "Datadog timeout" in err and "will retry" in err
    assert "#3 skipped: site unresolvable" in err
    assert "#4 unchanged" in err
```

- [ ] **Step 2: Run — expect failures (run_iteration not defined)**

Run: `pytest tests/test_watcher.py -v -k run_iteration`
Expected: FAIL with `ImportError: cannot import name 'run_iteration'`.

- [ ] **Step 3: Implement `run_iteration`**

Add these imports to the top of `triage_cli/watcher.py` (alongside the
existing imports from Task 4):

```python
import time
from datetime import timedelta

from triage_cli import extract, pipeline, render
from triage_cli.datadog import DatadogClient
from triage_cli.zendesk import ZendeskClient
```

Then append to `triage_cli/watcher.py`:

```python
def _now_local_hms() -> str:
    return datetime.now().strftime("%H:%M:%S")


def _emit(msg: str) -> None:
    print(msg, file=sys.stderr, flush=True)


def run_iteration(
    zd: ZendeskClient,
    sites: list[Any],
    state: State,
    opts: WatcherOptions,
    backfill_cutoff: datetime,
    dd_client: DatadogClient | None,
) -> State:
    """Run one poll-and-triage pass over the view. Returns the updated state."""
    triaged_map: dict[str, str] = dict(state.get("triaged") or {})
    new_state: State = {"version": STATE_VERSION, "triaged": triaged_map}

    try:
        view_ids = zd.list_view_ticket_ids(opts.view_id)
    except RuntimeError as e:
        _emit(f"[{_now_local_hms()}] iteration aborted: {e}")
        return new_state

    for tid in view_ids:
        key = str(tid)
        try:
            ticket = zd.get_ticket(tid)
        except (RuntimeError, ValueError) as e:
            _emit(f"[{_now_local_hms()}] #{tid} failed: {e} (will retry)")
            continue

        stored = triaged_map.get(key)
        if not should_triage(ticket, new_state, backfill_cutoff):
            if stored is None:
                # First-run silent backfill: mark as seen, no note.
                triaged_map[key] = ticket.updated_at.isoformat()
            else:
                _emit(f"[{_now_local_hms()}] #{tid} unchanged")
            continue

        try:
            site_entry, _strategy = extract.lookup_site(ticket, sites)
        except ValueError as e:
            _emit(f"[{_now_local_hms()}] #{tid} failed: {e} (will retry)")
            continue
        if site_entry is None:
            _emit(f"[{_now_local_hms()}] #{tid} skipped: site unresolvable")
            continue

        try:
            markdown = pipeline.triage_one(
                ticket,
                site_entry,
                dd_client=dd_client,
                window_minutes=opts.window_minutes,
                levels=opts.levels,
                at=None,
                verbose=opts.verbose,
                show_spinner=False,
            )
        except (RuntimeError, ValueError) as e:
            _emit(f"[{_now_local_hms()}] #{tid} failed: {e} (will retry)")
            continue

        path = render.save_note(markdown, ticket.id)
        _emit(f"[{_now_local_hms()}] #{tid} triaged → {path}")
        if opts.print_notes:
            print(markdown, flush=True)
            print("---", flush=True)
        triaged_map[key] = ticket.updated_at.isoformat()

    return new_state
```

Tests will monkeypatch `triage_cli.pipeline.triage_one` and
`triage_cli.render.save_note` directly on the source modules; this works
because we look up the attributes via the imported module reference each
call rather than binding them locally.

- [ ] **Step 4: Run the run_iteration tests — expect 2 passed**

Run: `pytest tests/test_watcher.py -v -k run_iteration`
Expected: 2 passed.

- [ ] **Step 5: Run all watcher tests**

Run: `pytest tests/test_watcher.py -v`
Expected: 11 passed.

- [ ] **Step 6: Implement `run_watch`**

Append to `triage_cli/watcher.py`:

```python
def run_watch(opts: WatcherOptions) -> None:
    """Main loop. Polls a view, triages new/updated tickets, sleeps, repeats.

    Exits cleanly on KeyboardInterrupt. On unrecoverable startup errors
    (cannot load site map, missing Zendesk env), raises RuntimeError so
    the CLI can print and exit.
    """
    sites = extract.load_site_map(Path("data/cnc-map.json"))
    state = load_state(opts.state_file)
    cutoff = (
        datetime.now(timezone.utc) - timedelta(hours=opts.backfill_hours)
        if math.isfinite(opts.backfill_hours)
        else datetime.min.replace(tzinfo=timezone.utc)
    )

    iteration = 0
    try:
        while True:
            iteration += 1
            _emit(f"[{_now_local_hms()}] iteration {iteration} start (view={opts.view_id})")
            with ZendeskClient.from_env() as zd:
                if opts.no_logs:
                    state = run_iteration(zd, sites, state, opts, cutoff, dd_client=None)
                else:
                    with DatadogClient.from_env() as dd:
                        state = run_iteration(zd, sites, state, opts, cutoff, dd_client=dd)
            save_state(opts.state_file, prune_state(state))
            _emit(f"[{_now_local_hms()}] iteration {iteration} done; sleeping {opts.interval}s")
            time.sleep(opts.interval)
    except KeyboardInterrupt:
        _emit(f"[{_now_local_hms()}] watcher stopped (Ctrl-C)")
        save_state(opts.state_file, prune_state(state))
```

- [ ] **Step 7: Run full suite**

Run: `pytest tests/ -q`
Expected: 46 passed (32 + 1 pipeline + 2 zendesk + 11 watcher).

- [ ] **Step 8: Commit**

```bash
git add triage_cli/watcher.py tests/test_watcher.py
git commit -m "$(cat <<'EOF'
feat(watcher): add run_iteration and run_watch loop

run_iteration walks view ticket IDs, decides via should_triage, runs
pipeline.triage_one for each new/updated ticket, and emits one
status line per ticket to stderr (triaged / unchanged / skipped /
failed). State is updated only for successful triages (and silently
for tickets older than the backfill cutoff on first run). run_watch
wraps the iteration in a sleep-after-iteration loop with clean
KeyboardInterrupt handling.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Wire `cli.watch` and update `.gitignore`

**Files:**
- Modify: `triage_cli/cli.py` (add `watch` command + `--backfill` parser)
- Modify: `.gitignore` (add `data/watcher-state-*.json`)

The CLI is a thin Typer command that parses flags, builds `WatcherOptions`, and calls `watcher.run_watch`.

- [ ] **Step 1: Add the `--backfill` parser and `watch` command to `cli.py`**

Edit `triage_cli/cli.py`. Add a parser helper near the other `_parse_*` functions:

```python
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
```

Add the `watch` command near the bottom of the module (after `triage`, before `build_map`):

```python
@app.command()
def watch(
    view: int = typer.Option(..., "--view", help="Zendesk view ID to watch"),
    interval: int = typer.Option(300, "--interval", min=10, help="Seconds to sleep after each iteration"),
    state_file: Path | None = typer.Option(None, "--state-file", help="State file path (default: data/watcher-state-<view>.json)"),
    backfill: str = typer.Option("24h", "--backfill", help="Initial backfill horizon: inf, 0, Nh, Nd"),
    window_minutes: int = typer.Option(30, "--window-minutes", min=1),
    levels: str = typer.Option("error,warn", "--levels"),
    no_logs: bool = typer.Option(False, "--no-logs", help="Skip Datadog; ticket-content-only triage"),
    print_notes: bool = typer.Option(False, "--print-notes", help="Also print full markdown to stdout"),
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
    resolved_state = state_file if state_file is not None else Path(f"data/watcher-state-{view}.json")

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
    except RuntimeError as e:
        _die(str(e))
```

- [ ] **Step 2: Add the gitignore wildcard**

Edit `.gitignore`. Add a line after the existing entries:

```
data/watcher-state-*.json
```

- [ ] **Step 3: Verify the CLI surface**

Run: `python -m triage_cli.cli watch --help` (or `triage-cli watch --help`)
Expected: Help text shows all flags (`--view`, `--interval`, `--state-file`, `--backfill`, `--window-minutes`, `--levels`, `--no-logs`, `--print-notes`, `--verbose`).

- [ ] **Step 4: Run the full suite**

Run: `pytest tests/ -q`
Expected: 46 passed.

- [ ] **Step 5: Commit**

```bash
git add triage_cli/cli.py .gitignore
git commit -m "$(cat <<'EOF'
feat(cli): add `watch` subcommand for view-driven triage loop

Wires watcher.run_watch behind a Typer command. --backfill accepts
'inf', '0', 'Nh', 'Nd' and defaults to 24h. State file defaults to
data/watcher-state-<view>.json (ignored via wildcard) so two
parallel watchers on different views don't collide.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Documentation

**Files:**
- Modify: `README.md` (add "Watching a Zendesk view" section + project layout)
- Create: `docs/runbooks/06-watching-a-view.md`

- [ ] **Step 1: Read the current README to find insertion points**

Read `README.md`. Locate (a) the section that describes the existing `triage` command and (b) the project layout block. The new "Watching a Zendesk view" section goes after the `triage` section and before "Limitations" (or its equivalent).

- [ ] **Step 2: Add the README section**

Insert this section into `README.md` immediately after the "Triaging a ticket" section and before "Limitations":

```markdown
## Watching a Zendesk view

Run a polling loop that triages every new or updated ticket in a Zendesk view:

```bash
triage-cli watch --view 12345
```

This will:
- Poll the view every 5 minutes (`--interval 300`).
- On first run, triage every ticket whose `updated_at` is within the last 24
  hours (`--backfill 24h`) and silently mark older tickets as "seen".
- Save each note to `./triage-notes/<ticket-id>-<timestamp>.md`.
- Emit one structured status line per ticket to stderr.
- Persist state to `data/watcher-state-<view-id>.json` so restarts pick up
  where they left off.

Common flags:
- `--backfill 0` — watermark mode; only future updates trigger notes.
- `--backfill inf` — triage every ticket in the view on first run.
- `--print-notes` — also stream the full markdown to stdout.
- `--no-logs` — skip Datadog (ticket-content-only triage).

See `docs/runbooks/06-watching-a-view.md` for a full operator runbook.
```

In the project-layout block in the README, add the two new modules. If the existing layout looks like this:

```
triage_cli/
├── cli.py
├── datadog.py
├── extract.py
├── llm.py
├── models.py
├── render.py
└── zendesk.py
```

Update it to:

```
triage_cli/
├── cli.py
├── datadog.py
├── extract.py
├── llm.py
├── models.py
├── pipeline.py
├── render.py
├── watcher.py
└── zendesk.py
```

- [ ] **Step 3: Create the runbook**

Create `docs/runbooks/06-watching-a-view.md`:

```markdown
# Runbook 06: Watching a Zendesk view

## When to use

You want continuous triage notes for tickets in a specific Zendesk view —
say, "Open Tier-1 incidents" — without running `triage-cli triage` by hand
for every new ticket or comment update.

## Prerequisites

- The standard `triage-cli` env (`ZENDESK_*`, `DATADOG_*`, `ANTHROPIC_MODEL`).
- The numeric Zendesk view ID. Find it in the Zendesk UI: open the view and
  the URL ends in `/views/<id>`.
- A built site map at `data/cnc-map.json` (run `triage-cli build-map` if
  missing).

## First run

```bash
triage-cli watch --view 12345
```

Default behavior:

- **Backfill horizon:** 24 hours. Every ticket whose `updated_at` is within
  the last 24 hours gets triaged on first run; older tickets are recorded as
  "already seen" without producing notes.
- **Interval:** sleep 300 seconds between iterations.
- **Output:** notes saved to `./triage-notes/`. stderr carries one status
  line per ticket. stdout is silent.

Want every ticket in the view triaged on first run? `--backfill inf`.
Want no notes at all on first run, only future updates? `--backfill 0`.

## Reading the stderr stream

```
[14:32:01] iteration 1 start (view=12345)
[14:32:04] #98765 triaged → triage-notes/98765-20260507T143204Z.md
[14:32:31] #98766 skipped: site unresolvable
[14:33:08] #98767 failed: Datadog timeout (will retry)
[14:33:09] #98768 unchanged
[14:33:09] iteration 1 done; sleeping 300s
```

Status verbs:
- `triaged` — note generated and saved.
- `unchanged` — ticket's `updated_at` matches what we triaged before.
- `skipped: site unresolvable` — ticket subject/description/org didn't match
  the site map. State is **not** marked, so the next time this ticket gets
  touched (or you fix the site map) it'll retry.
- `failed: <reason> (will retry)` — transient error (Datadog timeout, Claude
  rate limit, Zendesk 5xx). State is **not** marked; the next iteration
  retries.

## State file

Path: `data/watcher-state-<view-id>.json` (default) or whatever you passed to
`--state-file`. Schema:

```json
{
  "version": 1,
  "triaged": {
    "98765": "2026-05-07T14:32:04+00:00"
  }
}
```

The watcher prunes the file to the 1000 most-recent entries on every save,
so it doesn't grow unbounded.

## Recovering from accidental state deletion

If you delete the state file, the watcher treats every ticket within the
backfill horizon as "new" and re-triages them. Cost: `2 × N` Claude calls
where `N` is the number of tickets in the horizon. To avoid the burst, run
once with `--backfill 0` after restoring; that re-marks every ticket in the
view as "seen" without producing notes, then your next watcher invocation
catches only future updates.

## Stopping the watcher

`Ctrl-C` exits cleanly: the current iteration finishes its in-flight ticket
(no abort mid-LLM-call), state is saved, and the process exits 0.

## Two views at once

Run two watchers in two terminals; their default state files are different
(`data/watcher-state-123.json` vs. `data/watcher-state-456.json`), so they
do not collide.
```

- [ ] **Step 4: Run the suite to confirm nothing regressed**

Run: `pytest tests/ -q`
Expected: 46 passed.

- [ ] **Step 5: Commit**

```bash
git add README.md docs/runbooks/06-watching-a-view.md
git commit -m "$(cat <<'EOF'
docs: cover watch mode in README + runbook 06

Adds "Watching a Zendesk view" to the README with the common flags
and a pointer to the new runbook. Runbook 06 walks operators through
first-run backfill behavior, the stderr status stream, state file
schema, and how to recover from accidental state deletion.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-review checklist

- [ ] All seven tasks committed.
- [ ] `pytest tests/ -q` reports 46 passed.
- [ ] `triage-cli triage <id> --no-logs` still works end-to-end on a known ticket (manual smoke).
- [ ] `triage-cli watch --view <id> --backfill 0 --interval 30` runs one iteration, prints `iteration 1 done`, sleeps, and exits cleanly on Ctrl-C (manual smoke).
- [ ] `data/watcher-state-<view>.json` is created and ignored by git.
- [ ] `ruff check .` is no worse than the pre-task baseline.
