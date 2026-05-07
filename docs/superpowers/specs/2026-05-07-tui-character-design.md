# v2: TUI character & inbox

**Status:** locked for implementation
**Date:** 2026-05-07
**Baseline commit:** `1bf8452` (fix(watcher): pre-merge fixes from whole-branch review)
**Cadence:** weekend sprint, three back-to-back phases, smoke-test target Sunday.

## Goal

Two changes shipped together, separable by phase:

1. **Output-first.** Replace the LLM's freeform-markdown contract with a structured `TriageReport` Pydantic model. The renderer becomes TTY-aware (Rich layout when interactive, raw markdown when piped). Saved notes get a `.json` sidecar. `triage` and `watch` produce a richer, paste-ready output without changing how they're invoked.

2. **Inbox TUI.** New `triage-cli inbox --view <id>` Textual app: a vertically-split inbox of ticket rows on the left and the selected `TriageReport` rendered on the right. Reuses the existing `watcher.run_iteration` loop via optional callbacks; `watch` and `triage` retain their command surface and behavior (only the rendered output changes, in phase 1).

The product principle: the user should never need two windows open to triage. The terminal is sufficient.

## Non-goals

- A single-ticket "investigation cockpit" with a step rail / timeline / SIP flow diagram. The schema is small enough not to need it; revisit if real friction emerges.
- Themes / `--theme` flag. One calm Rich default; honor `NO_COLOR`.
- Voice modes (`--voice calm/dry/noc-goblin`). Personality lives in stderr status microcopy only.
- Reader-mode log toggles (wrap/group/highlight). Datadog's UI does this; the CLI returns a curated slice.
- Command palette, search filter, ad-hoc re-triage with overrides. Defer to v3 if real friction emerges.
- File-locking on the watcher state file. Single-user tool; last-writer-wins is acceptable.
- Two-process inbox (separate `watch` writer + `inbox` reader sharing state). One process; revisit only if the watcher needs to live on a different host.

## Layouts considered

Three inbox layouts were prototyped during brainstorming. The chosen layout (vertical split with the list always visible) and the two rejected alternatives (drill-in detail screen, and list + tabbed detail) are preserved at `docs/superpowers/specs/assets/2026-05-07-tui-character/inbox-layouts-considered.html` for re-evaluation if A doesn't land in practice.

## Architecture overview

```
cli.py (thin)
  ├── triage(...)        → pipeline.triage_one() → TriageReport
  │                        → render.print_note(report) [TTY-aware]
  │                        → render.save_note(report, id) [.md + .json] when --save
  ├── watch(...)         → watcher.run_watch() unchanged externally
  │                        (run_iteration internals updated for TriageReport)
  └── inbox(...)         → InboxApp(opts).run()
                            ├── on_mount: hydrate.recent_reports(notes_dir, 24h)
                            ├── set_interval: _poll_tick → asyncio.to_thread → watcher.run_iteration
                            └── callbacks bridge worker thread → UI via call_from_thread
```

### Module budget

| Module | New / Modified | Approx LOC | Note |
|---|---|---|---|
| `models.py` | modified | +60 | adds `TriageReport`, `LLMTriageOutput`, `EvidenceItem`, `TimeWindow`, `Confidence` |
| `llm.py` | modified | +20 | new prompt; JSON-mode parse with one retry |
| `pipeline.py` | modified | +15 | returns `TriageReport`; attaches pipeline-derived fields |
| `render.py` | modified | +120 | grows past 150-line budget — split if needed: `render.py` (public) + `render_rich.py` (Rich layout helpers) |
| `cli.py` | modified | +40 | new `inbox` subcommand; `triage`/`watch` consume `TriageReport` |
| `watcher.py` | modified | +30 | adds optional callbacks to `run_iteration`; behavior unchanged when callbacks are `None` |
| `inbox/__init__.py` | new | ~5 | re-exports `InboxApp` |
| `inbox/app.py` | new | ~140 | Textual `App` — bindings, layout, poll worker, single-flight guard |
| `inbox/widgets.py` | new | ~120 | `TicketListWidget` (DataTable), `ReportPaneWidget` (Static with Rich renderable) |
| `inbox/hydrate.py` | new | ~50 | scan `triage-notes/*.json`, dedupe latest-per-ticket within 24h |
| `inbox/clipboard.py` | new | ~40 | best-effort `wl-copy` / `xclip` / `pbcopy` chain |

### Dependency additions

- `rich>=13` — phase 1
- `textual>=0.80` — phase 2

## Phase 1 — output-first

### Schema (`models.py`)

```python
Confidence = Literal["low", "medium", "high"]

class TimeWindow(BaseModel):
    start: datetime  # tz-aware UTC
    end: datetime    # tz-aware UTC

class EvidenceItem(BaseModel):
    timestamp: datetime | None = None  # None when from ticket text, not logs
    service: str | None = None
    message: str

class LLMTriageOutput(BaseModel):
    finding: str
    confidence: Confidence
    evidence: list[EvidenceItem]
    suggested_note: str
    next_checks: list[str] = []
    unknowns: list[str] = []

class TriageReport(LLMTriageOutput):
    ticket_id: int
    site_name: str
    window: TimeWindow
    sources: list[str]
    log_event_count: int
    generated_at: datetime
```

`TriageReport` extends `LLMTriageOutput` (inheritance, not composition) so consumers see one flat namespace.

### Prompt change (`llm.py`)

Replaces the existing four-section markdown prompt. Asks for a single JSON object matching the schema, with confidence-calibration rules and the existing discipline rules ("don't invent log lines", "empty findings are valid").

```
You are a triage assistant for a Network Engineer working on the Carbyne APEX
NG911/E911 platform at Axon. Return a single JSON object — no prose, no
fences required but a ```json fence is acceptable — matching this schema:

{
  "finding":         "<one or two sentences. What's likely wrong. No padding.>",
  "confidence":      "low" | "medium" | "high",
  "evidence":        [{"timestamp": "<ISO 8601 UTC or null>",
                       "service": "<service name or null>",
                       "message": "<terse, factual>"}],
  "suggested_note":  "<paste-ready Zendesk internal note. Markdown allowed.>",
  "next_checks":     ["<concrete verification step>", ...],
  "unknowns":        ["<what you couldn't determine>", ...]
}

Confidence calibration:
- "high":   logs and ticket agree on a specific failure mode.
- "medium": logs are consistent with one cause but don't prove it.
- "low":    logs absent, ambiguous, or contradict the ticket.

Rules:
- Do not invent log lines, error codes, ticket IDs, or past incidents.
- Empty arrays are preferred over filler.
- If you would hedge three times in finding, the right field is confidence:"low".
```

### LLM call with single retry

```python
async def triage(bundle: TriageBundle, model: str | None = None,
                 *, verbose: bool = False) -> LLMTriageOutput:
    resolved = _resolve_model(model)
    raw = await _collect_text(bundle.as_user_message(), TRIAGE_SYSTEM_PROMPT, resolved)
    try:
        return LLMTriageOutput.model_validate_json(_strip_code_fence(raw.strip()))
    except (json.JSONDecodeError, ValidationError) as e:
        if verbose:
            logger.warning("triage: first attempt returned invalid JSON; retrying. %s", e)
        retry_prompt = (
            bundle.as_user_message()
            + "\n\nReturn ONLY a single valid JSON object matching the schema. No prose."
        )
        raw2 = await _collect_text(retry_prompt, TRIAGE_SYSTEM_PROMPT, resolved)
        try:
            return LLMTriageOutput.model_validate_json(_strip_code_fence(raw2.strip()))
        except (json.JSONDecodeError, ValidationError) as e2:
            raise RuntimeError(f"LLM returned invalid TriageReport JSON after retry: {e2}") from e2
```

The `verbose` flag is plumbed `cli` → `pipeline.triage_one(verbose=...)` → `llm.triage(verbose=...)` (parameter already exists on the pipeline). The retry log goes to the existing logger; in `inbox`, that's redirected to `data/inbox-<view>.log` (see error handling).

### Pipeline assembly (`pipeline.py`)

```python
async def triage_one(...) -> TriageReport:
    ...
    llm_out: LLMTriageOutput = await llm.triage(bundle, verbose=verbose)
    return TriageReport(
        **llm_out.model_dump(),
        ticket_id=ticket.id,
        site_name=site_entry.site_name,
        window=TimeWindow(start=anchor - delta, end=anchor + delta),
        sources=["zendesk"] + (["datadog"] if logs is not None else []),
        log_event_count=len(logs or []),
        generated_at=datetime.now(timezone.utc),
    )
```

### Renderer (`render.py`)

Three public functions plus a Rich-layout helper consumed by both the TTY render and the inbox's `ReportPaneWidget`:

```python
def to_markdown(report: TriageReport) -> str: ...
def rich_layout(report: TriageReport) -> ConsoleRenderable: ...   # Group of Panels
def print_note(report: TriageReport, *, console: Console | None = None) -> None: ...
def save_note(
    report: TriageReport,
    ticket_id: int,
    output_dir: Path | None = None,
) -> tuple[Path, Path]: ...   # (markdown_path, json_path)
```

`print_note` decides:
- If `console` is provided, render via `console.print(rich_layout(report))`.
- Else if `sys.stdout.isatty()` and `os.getenv("NO_COLOR")` is unset: build a `Console()` and render the Rich layout.
- Else: `typer.echo(to_markdown(report))` — preserves the pipeable-stdout contract.

`save_note` writes both `<id>-<ts>.md` (deterministic, diffable) and `<id>-<ts>.json` (`report.model_dump_json(indent=2)`). Sections with empty `next_checks` / `unknowns` are omitted from markdown — no empty headers.

### CLI wire-up

`cli.triage` and `cli.watch` are two-line diffs each: swap `markdown: str` for `report: TriageReport`, call new `print_note` / `save_note`. Verbose mode (`-v`) gains one extra stderr line printed after triage: `Confidence: medium · 8 events · sources: zendesk, datadog`.

### Microcopy (personality, phase 1)

The light NOC voice lives only in stderr status lines emitted during `triage` and `watch` runs. Examples:

```
Fetching ticket...
Mapping to a site...
Listening to the wire (Datadog, 14:00–14:15)...
Asking Claude what it thinks...
Done. 8 events, medium confidence.
```

The saved markdown and the rendered Rich layout stay formal — section names are `Finding`, `Evidence`, `Next Checks`, `Unknowns`, `Suggested Internal Note`. Anything paste-able into Zendesk is serious; anything ephemeral on screen can have voice.

## Phase 2 — inbox skeleton

### Watcher callback hook (`watcher.py`)

`run_iteration` grows four optional callbacks. `watch` passes `None` for all (current behavior preserved); `inbox` passes UI-updaters that bridge thread → UI via `app.call_from_thread`.

```python
def run_iteration(
    opts: WatcherOptions,
    state: State,
    *,
    on_view_listed: Callable[[list[int]], None] | None = None,
    on_progress:    Callable[[int, str], None] | None = None,    # (ticket_id, "triaging")
    on_complete:    Callable[[TriageReport], None] | None = None,
    on_failure:     Callable[[int, str], None] | None = None,    # (ticket_id, error_msg)
) -> State:
    ...
```

Callbacks fire as work happens, not at iteration end — so the inbox status icon flips ○ → → → ✓ live, one ticket at a time. State writes remain at iteration end via `os.replace` (unchanged).

State file shape **does not change**. `pending` is computed from `view_ids - triaged_ids - in_progress_ids - failed_ids` at render time. `triaging` and `failed` are tracked in inbox memory only; failures don't persist across watcher runs because the watcher already retries on next poll.

### Inbox app (`inbox/`)

```python
class InboxApp(App):
    BINDINGS = [
        Binding("up,k",       "cursor_up",   "↑"),
        Binding("down,j",     "cursor_down", "↓"),
        Binding("enter",      "focus_detail","focus"),
        Binding("escape",     "focus_list",  show=False),
        Binding("r",          "refresh",     "refresh"),
        Binding("y",          "copy_note",   "copy note"),
        Binding("o",          "open_zendesk","open"),
        Binding("q,ctrl+c",   "quit",        "quit"),
    ]

    def __init__(self, opts: WatcherOptions): ...

    def compose(self):
        yield Header()
        with Horizontal():
            yield TicketListWidget(id="list")
            yield ReportPaneWidget(id="detail")
        yield Footer()

    async def on_mount(self):
        for r in hydrate.recent_reports(self.opts.notes_dir, hours=24):
            self._rows[r.ticket_id] = RowEntry(report=r, status="triaged")
        self._refresh_list()
        self.set_interval(self.opts.poll_seconds, self._poll_tick)
        await self._poll_tick()

    async def _poll_tick(self):
        if self._polling: return
        self._polling = True
        try:
            await asyncio.to_thread(self._run_iteration_blocking)
        finally:
            self._polling = False
```

### State model in the inbox

```python
@dataclass
class RowEntry:
    ticket_id: int
    status: Literal["triaged", "triaging", "pending", "failed"]
    report: TriageReport | None
    site_hint: str | None = None
    failure_reason: str | None = None
```

Sort order: `(status_priority, generated_at desc)` — `triaging` rows float to the top, then fresh `triaged`, then older `triaged`, then `pending`, then `failed` last.

### Status icons

`✓` triaged · `→` triaging · `○` pending · `✗` failed · `◉` selected (cursor row).

### Hydration (`inbox/hydrate.py`)

```python
def recent_reports(notes_dir: Path, *, hours: int = 24) -> list[TriageReport]:
    """Read JSON sidecars, filter to last `hours`, dedupe latest-per-ticket."""
```

Corrupt sidecars are skipped silently; `logger.warning` if `--verbose`. One bad file does not break startup.

### Clipboard (`inbox/clipboard.py`)

Best-effort fallback chain: `wl-copy` → `xclip -selection clipboard` → `pbcopy`. Returns `False` if none are available; the action handler shows a non-blocking `notify(severity="warning")` with the install hint.

## Phase 3 — inbox live

### Action behaviors

- **`y` copy note** — `copy_to_clipboard(selected.report.suggested_note)`; toast on success or install hint on failure.
- **`o` open in Zendesk** — Build `https://{ZENDESK_SUBDOMAIN}.zendesk.com/agent/tickets/{id}`; call `webbrowser.open(url)` *and* show URL in a 10s notification regardless of return value (lets headless-SSH users copy from the toast).
- **`r` refresh** — Cancel pending timer tick; trigger `_poll_tick()` immediately; timer resumes its cadence.
- **`enter` focus_detail** / **`escape` focus_list** — Move keyboard focus between panes so `↑↓`/`pgup`/`pgdn` scroll the report instead of the list when reading.

### Empty / loading / error states

- **Empty list:** centered `Static` reading `"No tickets matched. Last poll: HH:MM. Next: HH:MM."`
- **Detail with no selection:** dim `"Select a ticket to view its report."`
- **Failed row:** `✗` + `failure_reason` truncated to ~50 chars in the summary column. Updates in place on next-poll success.
- **Clipboard / Zendesk subdomain missing:** non-blocking `notify(severity="warning")`; app keeps running.

### CLI entry point

```python
@app.command()
def inbox(
    view: int = typer.Option(..., "--view", help="Zendesk view ID to monitor"),
    poll: int = typer.Option(60, "--poll", help="Seconds between polls"),
    backfill: str = typer.Option("0", "--backfill", help="As in `watch`"),
    window_minutes: int = typer.Option(15, "--window-minutes"),
    levels: str = typer.Option("error,warn", "--levels"),
    verbose: bool = typer.Option(False, "--verbose", "-v"),
):
    """Launch the interactive inbox TUI for a Zendesk view."""
    if not sys.stdout.isatty():
        _die("inbox requires an interactive terminal. Use `watch` for headless runs.")
    opts = _build_watcher_options(view, poll, backfill, window_minutes, levels, verbose)
    InboxApp(opts).run()
```

Same flag vocabulary as `watch` for overlapping fields. No `--save` flag — saving is implicit (the inbox needs the JSON sidecars).

## Persistence

| Artifact | Path | Format | Written by |
|---|---|---|---|
| Markdown note | `triage-notes/<id>-<ts>.md` | Deterministic markdown from `to_markdown(report)` | `render.save_note` |
| JSON sidecar | `triage-notes/<id>-<ts>.json` | `report.model_dump_json(indent=2)` | `render.save_note` |
| Watcher state | `data/watcher-state-<view>.json` | `{version: 1, triaged: {id: updated_at}}` | `watcher.save_state` (unchanged) |
| Inbox runtime log | `data/inbox-<view>.log` | Python `logging` (FileHandler, WARNING+) | `inbox.app` setup |

Markdown is the human artifact; JSON is the machine artifact. Both are written together; `save_note` returns both paths.

## Error handling

| Condition | Behavior |
|---|---|
| LLM returns malformed JSON | One silent retry with stricter nudge; second failure raises `RuntimeError` into existing failure path. `--verbose` logs both attempts. |
| LLM transport error | Existing path: `RuntimeError` propagates, watcher marks row failed and retries on next poll. |
| Slow Zendesk/Datadog vs poll interval | Single-flight guard in `_poll_tick` — if a poll is in progress, the next tick is skipped (silent, or `notify` if verbose). |
| Corrupt JSON sidecar at hydrate | Skipped silently; `logger.warning` if verbose. |
| Disk write failure on `save_note` | Existing watcher path catches `OSError`, marks row failed, retries on next poll. |
| Clipboard tool missing | `notify(severity="warning", "Install wl-copy or xclip to copy")`; app continues. |
| `ZENDESK_SUBDOMAIN` env var unset | `notify(severity="warning")`; URL not opened. |
| Non-TTY launch | `_die("inbox requires an interactive terminal")` before Textual takes over. |
| State file version mismatch at startup | Existing watcher path raises; surfaces as a `_die` message before TUI launch. |
| `Ctrl-C` mid-poll | Textual cancels the event loop; the `to_thread` worker completes its current iteration (the final `save_state` is atomic). At-most-one extra ticket may be triaged after the UI closes; its sidecar is picked up on next launch via hydration. |

## Testing

- `tests/test_models.py` — `TriageReport` JSON round-trip; reject bad confidence values; tz-aware datetime enforcement.
- `tests/test_llm.py` — patch `_collect_text`; canned-JSON / fenced-JSON / malformed-JSON paths; retry-on-malformed; raise-after-retry.
- `tests/test_render.py` — `to_markdown` is deterministic; empty `next_checks` / `unknowns` omit their sections; `print_note` selects Rich vs raw based on `Console(force_terminal=...)` injection.
- `tests/test_pipeline.py` — `triage_one` returns `TriageReport`; pipeline-derived fields (site, window, sources, event count) populated correctly.
- `tests/test_hydrate.py` — fixture sidecars; cutoff filtering; corrupt-file skip; latest-per-ticket dedup.
- `tests/test_clipboard.py` — patch `subprocess.run`; verify fallback chain order; `False` when nothing is available.
- `tests/test_inbox_state.py` — `RowEntry` ordering; status transitions; `pending` derivation from `view_ids - tracked`.
- `tests/test_watcher_callbacks.py` — `run_iteration` invokes callbacks in the right order on a mocked pipeline; `watch` (no callbacks) regression check (current behavior byte-identical).

**No Textual snapshot test** for v2 — the app is thin, the per-widget tests + the callback contract test cover the moving parts. Manual smoke before declaring done.

Tests stay sealed off from live APIs (existing project rule). Zendesk/Datadog/Agent SDK are stubbed/monkeypatched.

## Sequencing — work blocks

Three back-to-back phases, weekend cadence, one commit per phase as a checkpoint.

**Phase 1 (~2–3 hr) — output-first.** `models.py`, `llm.py`, `pipeline.py`, `render.py`, `cli.py` updates. `+rich>=13`. Tests for models / llm / render / pipeline. End state: `triage <id>` produces Rich-rendered output; saved notes have `.json` sidecars; `watch` prints structured output.

**Phase 2 (~2–3 hr) — inbox skeleton.** `watcher.py` callback addition; `inbox/` sub-package (hydrate, clipboard, widgets, app shell). `+textual>=0.80`. Tests for hydrate / clipboard / watcher callbacks / inbox state. End state: `triage-cli inbox --view <id>` launches and shows 24h hydration; no live polling yet.

**Phase 3 (~1–2 hr) — inbox live.** Wire `_poll_tick` with single-flight guard, all keybindings, file logger, non-TTY guard, empty-state widgets, runbook entry. End state: feature complete.

Sunday smoke-test target: launch `triage-cli inbox --view <real-view-id>`, watch a real ticket flow through, copy a note, open in Zendesk.

## Open risks / future work

- **Rich-on-Textual rendering parity.** The same `rich_layout(report)` helper is consumed by both the TTY-headless `print_note` path and the inbox `ReportPaneWidget`. If Textual's renderable handling drifts from the headless `Console`, divergence is possible. Manual smoke covers it; revisit if the two outputs ever look different.
- **Module size.** `render.py` will likely outgrow the 150-line budget once Rich layout helpers land. If so, split into `render.py` (public API: `print_note`, `save_note`, `to_markdown`) + `render_rich.py` (Rich layout building blocks). Keep the public surface in `render.py`.
- **Concurrent inbox processes.** No file lock on `data/watcher-state-<view>.json`. Two inboxes against the same view will cause last-writer-wins on triaged-status. Documented; acceptable for single-user scope.
- **Headless `o` action.** `webbrowser.open` does the right thing on a desktop; on an SSH session with no `DISPLAY`, the URL toast is the fallback. If headless-SSH usage becomes the primary mode, consider auto-copying the URL to clipboard alongside the toast.
- **Re-evaluate v3 candidates after one week of use:** ad-hoc re-triage with window override (`n`, `e`), search filter (`/`), command palette. Don't pre-build them.
