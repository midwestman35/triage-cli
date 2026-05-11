# v2: `watch` mode for Zendesk views

**Status:** locked for implementation
**Date:** 2026-05-07
**Baseline commit:** `e100e3a` (feat(cli): add orbit spinner during slow operations)

## Goal

Add a `triage-cli watch --view <id>` subcommand that polls a Zendesk view and
runs the existing single-ticket triage pipeline against new or updated tickets,
saving notes to `./triage-notes/` and emitting a structured per-ticket status
stream to stderr. The loop runs until `Ctrl-C`.

## Non-goals (v3 candidates)

- Consecutive-failure backstop (auto-exit after N failed iterations).
- Parallel triage within an iteration (asyncio.gather with concurrency cap).
- In-iteration retry/backoff for transient errors (e.g. Anthropic 429).
- Per-view config files. All knobs are CLI flags.
- Notification fan-out (Slack, email). Files on disk + stderr only.

## Architecture

Three new units, two refactors. Each has one clear job and is testable in
isolation.

```
cli.py (thin)
  ├── triage(...)          → fetch ticket → resolve site (with prompt)
  │                          → pipeline.triage_one(ticket, site, opts) → markdown
  └── watch(...)           → watcher.run_watch(view_id, opts)
                              ├── watcher.load_state / save_state (atomic)
                              ├── watcher.should_triage(ticket, state, backfill_cutoff)
                              ├── watcher.run_iteration(...)
                              │     ├── fetch ticket → resolve site (no prompt)
                              │     └── pipeline.triage_one(ticket, site, opts)
                              └── time.sleep(interval)
```

### `triage_cli/pipeline.py` (new)

Extracted from the post-resolution part of `cli.triage`. One public function:

```python
def triage_one(
    ticket: Ticket,
    site_entry: SiteEntry,
    *,
    window_minutes: int,
    levels: list[str],
    no_logs: bool,
    at: datetime | None,
    verbose: bool,
    show_spinner: bool,
) -> str:
    """Run the triage pipeline for a fetched ticket and resolved site.

    Returns the rendered markdown.
    Raises RuntimeError on Datadog or Claude failure.
    Raises ValueError on validation failure (e.g. invalid window).
    """
```

- Inputs are the already-fetched `Ticket` and the already-resolved
  `SiteEntry`. Site resolution stays out of `pipeline` because the two
  callers have different policies for "no match": `cli.triage` prompts
  interactively, `watcher.run_iteration` skips with a status line.
- Owns the spinner + verbose calls for steps inside its boundary: anchor
  extraction, Datadog fetch, LLM triage call.
- No new exception class. Existing `RuntimeError`/`ValueError` flow up
  unchanged.
- The dead `ImportError` guard for `unicode_animations` is removed in this
  same commit (the package is a hard dependency in `pyproject.toml`).
- The duplicate "Querying Datadog for X" between `_vecho` and the spinner is
  resolved by dropping the `_vecho` line for that step (info is in the
  spinner) and keeping the post-fetch summary `_vecho` ("Pulled N log lines").

`cli.triage` shape after extraction:
1. Parse flags.
2. `ZendeskClient.get_ticket` (with spinner).
3. `extract.load_site_map` + `extract.lookup_site` (with interactive prompt
   on no_match unless `--no-interactive`).
4. `pipeline.triage_one(ticket, site_entry, ...)`.
5. `render.print_note` + optional `render.save_note`.

### `triage_cli/watcher.py` (new)

```python
@dataclass(frozen=True)
class WatcherOptions:
    view_id: int
    interval: int          # seconds between iterations (default 300)
    state_file: Path
    backfill_hours: float  # 0 = watermark, math.inf = full burst, default 24
    window_minutes: int
    levels: list[str]
    no_logs: bool
    print_notes: bool
    verbose: bool

State = dict        # see "State schema" below

def load_state(path: Path) -> State: ...
def save_state(path: Path, state: State) -> None: ...   # atomic: tempfile + os.replace
def should_triage(
    ticket: Ticket,                 # already fetched
    state: State,
    backfill_cutoff: datetime,
) -> bool: ...
def prune_state(state: State, max_entries: int = 1000) -> State: ...
def run_iteration(
    zd: ZendeskClient,
    sites: list[SiteEntry],
    state: State,
    opts: WatcherOptions,
    backfill_cutoff: datetime,
) -> State: ...                     # returns updated state
def run_watch(opts: WatcherOptions) -> None: ...   # the loop
```

`should_triage` is pure: input ticket + state + cutoff, output bool. No I/O,
trivially unit-testable.

`run_iteration` is the orchestration layer. It:

1. Lists view ticket IDs.
2. For each ID, fetches the ticket (`zd.get_ticket(id)` — already paginates
   comments, used today).
3. Decides via `should_triage`. If `False` because the ticket's `updated_at`
   matches state, prints `unchanged` status. If `False` because the ticket
   is older than the backfill cutoff on a first run, marks state silently
   and prints nothing.
4. On `True`: resolves site via `extract.lookup_site` (no `cnc`/`site`
   overrides; no interactive prompt). If `(None, "no_match")`, prints
   `skipped: site unresolvable` and **does not** mark state.
5. With a resolved site, calls `pipeline.triage_one`. On success: saves
   note via `render.save_note`, prints `triaged → <path>` status, updates
   state entry (`state["triaged"][str(id)] = ticket.updated_at.isoformat()`).
   If `--print-notes`, also writes the markdown to stdout.
6. On per-ticket failure (any `RuntimeError` or `ValueError` from
   `triage_one`): prints `failed: <reason> (will retry)` status, **does
   not** mark state. Continues to next ticket.
7. After the loop: `save_state(prune_state(state))`.

`run_watch` is dumb: iterate, sleep, iterate. `KeyboardInterrupt` exits
cleanly with status 0.

### `triage_cli/zendesk.py` (modified)

Add one method:

```python
def list_view_ticket_ids(self, view_id: int) -> list[int]:
    """Return ticket IDs in the given view, oldest-first by view ordering."""
```

Implementation: paginate `/views/{view_id}/tickets.json?page[size]=100`,
collect `ticket["id"]`, return as `list[int]`. Reuses the existing pagination
loop pattern from `_fetch_comments` (cursor + legacy fallback, MAX_PAGES cap).

**No `not_found_label` parameter on `_get`.** Instead, `list_view_ticket_ids`
catches `RuntimeError` from `_get` and re-raises with a view-flavored message
when the underlying error string contains `"not found"`. Cheap, contained.

### `triage_cli/cli.py` (modified)

`cli.triage` is restructured per the shape listed under `pipeline.py` above
(parse → fetch → resolve site → `triage_one` → render). The interactive site
prompt stays in `cli.triage` and never enters `pipeline` or `watcher`.

`cli.watch` is added:

```python
@app.command()
def watch(
    view: int = typer.Option(..., "--view", help="Zendesk view ID to watch"),
    interval: int = typer.Option(300, "--interval", min=10),
    state_file: Path | None = typer.Option(None, "--state-file"),
    backfill: str = typer.Option("24h", "--backfill", help="Initial backfill horizon: e.g. 24h, 7d, 0, inf"),
    window_minutes: int = typer.Option(30, "--window-minutes", min=1),
    levels: str = typer.Option("error,warn", "--levels"),
    no_logs: bool = typer.Option(False, "--no-logs"),
    print_notes: bool = typer.Option(False, "--print-notes", help="Also print full markdown to stdout"),
    verbose: bool = typer.Option(False, "--verbose", "-v"),
) -> None: ...
```

Default `state_file` = `Path(f"data/watcher-state-{view}.json")`.

`--backfill` parser accepts (case-insensitive):
- `inf` → `math.inf` (no cutoff; full burst).
- `0` → `0.0` hours (watermark mode; no notes on first run).
- `<int>h` → that many hours (e.g. `48h`).
- `<int>d` → days × 24 hours (e.g. `7d`).

Anything else is rejected with a `--help`-flavored message. Integers only;
fractional values (`1.5h`) are not supported in v2.

## State schema

```json
{
  "version": 1,
  "triaged": {
    "12345": "2026-05-07T13:12:04Z",
    "12346": "2026-05-07T13:14:11Z"
  }
}
```

- Keys are stringified ticket IDs (JSON object key constraint).
- Values are ISO 8601 UTC timestamps matching `Ticket.updated_at` (note: the
  current `Ticket` model in `models.py` does not expose `updated_at` — see
  "Required model change" below).
- `version: 1` lets us add migrations later without a panic. On
  version mismatch, `load_state` raises a clear `RuntimeError` with the
  detected version and the supported version.

### Pruning

`prune_state(state, max_entries=1000)` keeps the 1000 entries with the
most-recent timestamps and drops the rest. Called once per iteration before
`save_state`. Bounded growth, no time math.

### Atomic write

`save_state` writes to `<path>.tmp` then `os.replace(<path>.tmp, <path>)`.
Crash-safe.

## Required model change

`triage_cli/models.py::Ticket` currently has `created_at: datetime` but no
`updated_at`. Watch mode's fingerprint depends on `updated_at`, so we add it:

```python
class Ticket(BaseModel):
    ...
    created_at: datetime
    updated_at: datetime    # NEW
    ...
```

`zendesk.ZendeskClient.get_ticket` already receives `updated_at` from the
Zendesk API payload — we just need to parse it alongside `created_at`. No
existing test fixture asserts on the absence of `updated_at`, so this is a
backwards-compatible addition; existing JSON fixtures need one new key.

## Decision: re-triage policy

**Fingerprint by `updated_at`.** A ticket triages exactly once per distinct
`updated_at` we see for it. Tag-only changes still bump `updated_at` in
Zendesk and will cause a re-triage; that's acceptable cost for the simplicity
of "one timestamp tells us everything".

## Decision: first-run / backfill

**`--backfill 24h` default.** First-run computes
`cutoff = now - backfill_hours` and only triages view tickets whose
`updated_at >= cutoff`. Older tickets are silently entered into state as
"already seen" so they don't retrigger when the next iteration runs.

- `--backfill 0` → watermark mode. All current tickets entered as "seen", no
  notes produced; only future updates trigger triage.
- `--backfill inf` → no cutoff. Every ticket in the view gets a note
  (matches A-policy).

## Decision: per-ticket error handling

**Skip and continue.** Per-ticket exceptions (`RuntimeError`, `ValueError`)
are caught inside `run_iteration`'s loop and turned into a stderr status
line. State is not updated for that ticket, so the next iteration retries
once the ticket's `updated_at` advances (or immediately, if the failure was
transient on our side).

The "site unresolvable" case is handled before `pipeline.triage_one` runs:
`extract.lookup_site` returns `(None, "no_match")`, `run_iteration` prints
`skipped: site unresolvable`, and state is not updated. The ticket will be
revisited on its next `updated_at` change.

No in-iteration retry/backoff; we trust the next loop tick to handle
transients.

## Decision: output shape

- **stdout** is silent by default.
- `--print-notes` makes stdout emit the full markdown for each triaged
  ticket, separated by `\n---\n`. (Useful for `tee`/log capture.)
- **stderr** carries one status line per ticket, fixed format:

```
[14:32:01] #12345 triaged → triage-notes/12345-20260507T143201Z.md
[14:32:32] #12346 skipped: site unresolvable
[14:33:08] #12347 failed: Datadog timeout (will retry)
[14:33:08] #12348 unchanged
```

Status verbs: `triaged`, `skipped`, `failed`, `unchanged`. Time is local
HH:MM:SS to match operator wall clock.

## Decision: state file path

Default: `data/watcher-state-<view-id>.json`. Two `watch --view 123` and
`watch --view 456` invocations are independent, no locking. `.gitignore`
gets a wildcard entry: `data/watcher-state-*.json`. `--state-file` overrides
for backups or shared inspection.

## Decision: concurrency and pacing

- **Sequential triage within an iteration.** No asyncio.gather, no thread
  pool. v3 can add a concurrency cap if needed.
- **`--interval` = sleep-after-iteration.** A 300s interval means "sleep 300s
  *after* the iteration finishes". We do not try to maintain fixed cadence;
  there is no concept of overlapping iterations.

## Spinner cleanup (absorbed into pipeline-extraction commit)

Two cleanups roll into the same commit that extracts `pipeline.py`:

1. Drop the dead `try/except ImportError` for `unicode_animations` in
   `cli.py:24-27`. The package is a hard dependency in `pyproject.toml`; the
   guard is unreachable.
2. Resolve the verbose/spinner duplication of `"Querying Datadog for X"`.
   Drop the `_vecho` that prints the same site name the spinner is about to
   show; keep the post-fetch `_vecho("Pulled N log lines (truncated=...)")`.

These exist as leftover items from a previous codex handoff that never
shipped. They're absorbed naturally because the extraction touches every
spinner and `_vecho` call in the path.

## CLI flag summary

```
triage-cli watch --view <id>
                 [--interval 300]
                 [--state-file <path>]
                 [--backfill 24h | 0 | inf | Nh | Nd]
                 [--window-minutes 30]
                 [--levels error,warn]
                 [--no-logs]
                 [--print-notes]
                 [--verbose | -v]
```

## Test plan

Baseline: `tests/test_extract.py` has 32 tests today (`grep -c "^def test_"`).
Target: ~11 new in `tests/test_watcher.py` plus 1 in `tests/test_zendesk.py`
(or extending an existing module if one is added during implementation), so
the full suite ends near 43–45 tests.

New tests in `tests/test_watcher.py`:

1. `load_state` returns empty default when file is missing.
2. `load_state` round-trips through `save_state` (write → read → equality).
3. `save_state` is atomic (temp file is replaced; pre-existing file content
   is never partial — assert via no `<path>.tmp` left behind on success).
4. `load_state` raises clear error on `version` mismatch.
5. `should_triage` → True when ticket id is absent from state and
   `updated_at >= cutoff`.
6. `should_triage` → False when ticket id is absent but `updated_at < cutoff`
   (backfill horizon excludes it).
7. `should_triage` → False when state's stored `updated_at` equals the
   ticket's.
8. `should_triage` → True when ticket's `updated_at` is newer than state's.
9. `prune_state` keeps the N most-recent entries when over the cap.
10. `run_iteration` updates state only for tickets that successfully triaged
    (failed and skipped tickets stay unmarked). Mocks `pipeline.triage_one`.
11. `run_iteration` writes status lines to stderr in the documented format
    for each of: triaged, skipped, failed, unchanged.

New test in `tests/test_zendesk.py` (creating the file):

12. `list_view_ticket_ids` paginates correctly across two pages of mocked
    httpx responses and returns IDs in the order received.

## Documentation

- `README.md`: new section "Watching a Zendesk view" between the existing
  "Triaging a ticket" section and "Limitations". Project layout block gets
  `pipeline.py` and `watcher.py` added.
- `docs/runbooks/06-watching-a-view.md`: new runbook covering setup
  (finding a view ID), first-run backfill behavior, expected stderr stream,
  state file location, recovery from accidental state deletion, and Ctrl-C
  semantics.

## Implementation order (commit-by-commit)

1. **Add `Ticket.updated_at`** in `models.py`; update `zendesk.get_ticket`
   to parse it; update test fixtures. Tests pass.
2. **Extract `pipeline.triage_one`** into `triage_cli/pipeline.py`. Refactor
   `cli.triage` to call it. Absorb the spinner cleanups (dead guard, vecho
   dedupe) in this same commit. Tests pass; stdout/stderr behavior of
   `cli.triage` is unchanged.
3. **Add `ZendeskClient.list_view_ticket_ids`** + `tests/test_zendesk.py`
   covering pagination.
4. **Add `triage_cli/watcher.py`** with `WatcherOptions`, state functions,
   `should_triage`, `prune_state`, `run_iteration`, `run_watch`. Add
   `tests/test_watcher.py`.
5. **Wire `cli.watch`** with all flags. Update `.gitignore` for
   `data/watcher-state-*.json`.
6. **Docs**: README section + `docs/runbooks/06-watching-a-view.md`.

Each commit leaves the suite green and the existing `triage` command
unchanged from a user-visible standpoint.

## Risks and open questions

- **Anchor extraction cost.** Each new ticket calls Claude twice (anchor
  extract + main triage). A burst of 40 backfill tickets is 80 Claude calls.
  Acceptable for v2 because `--backfill` bounds the burst; flagged for v3
  if cost becomes painful (anchor extraction is best-effort and could fall
  back to `created_at` more aggressively).
- **Tag-only changes bump `updated_at`** and will cause a re-triage. We
  accept this for v2 simplicity. If it becomes noisy, v3 could fingerprint
  on comment count instead.
- **Zendesk view ordering** is configured per-view; we do not re-sort. If a
  view returns most-recent-first and is large, the backfill cutoff still
  applies correctly because `should_triage` compares `updated_at` against
  the cutoff per-ticket.
