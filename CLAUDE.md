# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Python 3.11+ CLI that investigates Zendesk tickets for the Carbyne APEX NG911/E911 platform. Five subcommands:

- `triage-cli investigate <id-or-url>` — guided investigation: fetch ticket → customer history → memory lookup → evidence intake → LLM assessment. Default entry point.
- `triage-cli triage <id-or-url>` — headless single-shot (same pipeline, no evidence prompts). Use in scripts or with `watch`.
- `triage-cli watch --view <id>` — long-running headless poll loop over a Zendesk view.
- `triage-cli inbox [--view ...]` — interactive Textual TUI report viewer; requires TTY.
- `triage-cli doctor` — checks env vars, credentials, and Zendesk connectivity; exits 0/1.
- `triage-cli build-map` — regenerates `data/cnc-map.json`.

`README.md` is the user-facing reference.

## LLM providers

Controlled by `LLM_PROVIDER` env var (default: `unleash`).

| Value | Provider | Required env vars |
|---|---|---|
| `unleash` | Unleash gateway | `UNLEASH_API_KEY`, `UNLEASH_ASSISTANT_ID` |
| `claude` | Claude Agent SDK | Claude CLI on PATH |
| `openai` | OpenAI Responses API | `OPENAI_API_KEY` |

## Memory layer

After every investigation, `MEMORY.md` and `data/memory.db` are updated with
the ticket ID, customer, subject, symptom, assessment, and resolution (if known).
Before the LLM call, the top-3 similar prior investigations are retrieved via
BM25 and injected as context.

To prune: edit `MEMORY.md` and delete entries. The FTS5 index rebuilds
automatically on the next investigation run.

## Common commands

```bash
# Install (editable, with dev deps)
pip install -e ".[dev]"

# Run all tests
pytest

# Run one test
pytest tests/test_extract.py::test_parse_ticket_id_from_url -q

# Lint
ruff check .

# Rebuild the site map after editing apex-cnc-inventory.md
triage-cli build-map

# Validate the read-only Zendesk boundary against your assigned queue
python scripts/certify_readonly_my_queue.py
```

Tests never hit live APIs — Zendesk, Datadog, and the Agent SDK are all stubbed/monkeypatched. Don't add network-touching tests.

## Architecture

### Pipeline ownership

`pipeline.investigate_one` is the shared async core called by all three entry points (`investigate`, `triage`, `watcher.run_iteration`). It drives: customer-history fetch → memory lookup → evidence intake → site resolution (optional) → Datadog enrichment (optional) → LLM call → save. **No I/O outside the injected clients.**

- `cli.investigate` — interactive entry point; adds evidence prompts and the drop-and-wait loop before calling `investigate_one`.
- `cli.triage` — headless entry point; calls `investigate_one` directly with no interaction.
- `watcher.run_iteration` — background poll; calls `investigate_one` with `SilentReporter`.
- `render` — stdout printing and saves to `./triage-notes/<id>-<ts>.md` + `.json`.

The `Reporter` protocol (`pipeline.Reporter`) decouples progress output from pipeline logic. Three implementations: `StderrReporter` (default), `SilentReporter` (tests/watcher), `TUIReporter` (--tui).

`triage_cli/investigation.py` builds `InvestigationSession` objects. Evidence models in `triage_cli/models.py`: `LocalFileEvidence`, `PastedEvidence`, `AttachmentEvidence`, `CustomerHistoryEvidence`, `InvestigationEvidence`, `InvestigationSession`, `MemoryEntry`, `MemoryContext`.

### Inbox TUI

`triage_cli/inbox/` is a [Textual](https://textual.textualize.io/) app over the same `WatcherOptions` that `watch` uses. `cli.inbox` builds the options, requires a TTY (refuses to launch without one), and hands off to `InboxApp.run()`. Logging is redirected to a per-view file printed at startup so the TUI itself stays clean.

Shared invariant with `watch`: state file is `data/watcher-state-<view-key>.json`, same shape for both. Don't fork the state schema between them.

### LLM access — provider-abstracted

`triage_cli/llm.py` dispatches to the provider selected by `LLM_PROVIDER` (default: `unleash`). Three provider implementations live in `triage_cli/providers/`:

- `unleash.py` — Unleash gateway (requires `UNLEASH_API_KEY` + `UNLEASH_ASSISTANT_ID`)
- `claude.py` — Claude Agent SDK; spawns Claude Code under the hood using OAuth. Requires `claude` CLI installed and authenticated.
- `openai.py` — OpenAI Responses API (requires `OPENAI_API_KEY`)

Two single-turn async calls in `llm.py`:
- `triage(bundle)` — main LLM assessment.
- `extract_anchor(ticket)` — best-effort timestamp extraction; returns `None` on failure.

Both are `await`ed from `pipeline.investigate_one`.

### Site map flow

`apex-cnc-inventory.md` (committed markdown tables) → `scripts/build_cnc_map.py` (invoked via `triage-cli build-map`) → `data/cnc-map.json` + `data/cnc-map-gaps.md`. Conversion rules (per-site table is canonical, master table fills gaps, blank-CNC rows go to gaps file, dedupe by CNC UUID, all retained entries must have a non-null `site_name`) are documented in `HANDOFF.md` — preserve them when editing `build_cnc_map.py`.

There is **no `confluence.py`** by design. Refreshing the inventory from Confluence is an out-of-band manual step. Do not add a Confluence module.

`extract.lookup_site` resolution priority: `--site` flag → `--cnc` flag → exact `friendly_name` match (case-insensitive) against `requester_org` → longest `site_name` substring in subject+description → longest `friendly_name` substring. The strategy name is returned for `--verbose` output.

### Anchor resolution

`extract.resolve_anchor` priority: `--at` flag (`AnchorSource.FLAG`) → LLM-extracted (`AnchorSource.EXTRACTED`) → `ticket.created_at` (`AnchorSource.CREATED_AT`). All datetimes are normalized to timezone-aware UTC inside the pipeline; naive inputs are treated as UTC (`extract._to_utc`, `datadog._ensure_aware`). Do not silently drop tzinfo when adding date logic.

### Datadog query

Site-level only: `<DD_CALL_CENTER_TAG>:<site_name> status:(<levels>)`. Window is `anchor ± window_minutes`, capped at 200 lines (`log_truncated=True` when at the cap). `site_name` is regex-validated (`^[a-zA-Z0-9._-]+$`) before query interpolation — do not loosen this. The `DD_STATION_TAG` env var is reserved for v2 station-level filtering and is read by no code today; leave it in `.env.example`.

### Watcher state

`data/watcher-state-<view-key>.json` has shape `{"version": 1, "triaged": {"<ticket-id>": "<iso updated_at>"}}`. `watcher.should_triage` is the pure decider (re-triage when `updated_at` advances; first-run silent backfill marks pre-cutoff tickets as seen with no note). State writes are atomic (tempfile + `os.replace`) and pruned to 1000 entries (`prune_state`) at the end of each iteration. Bumping `STATE_VERSION` requires a migration path, not a hard fail — current code raises on version mismatch, which is fine for a single-user tool but keep that in mind. The same state file is shared between `watch` and `inbox` for a given view.

### Read-only certification

`scripts/certify_readonly_my_queue.py` exercises the investigation flow against the authenticated user's assigned Zendesk queue without Datadog or LLM calls. Treat its passing run as the contract that the read-only boundary is intact — when adding code that touches Zendesk, run it before merging.

### Stdout vs stderr discipline

stdout is reserved for the rendered triage note so the output is pipeable. Everything else (verbose traces, spinners, watcher status lines, save-path notices, inbox log path) goes to stderr via `typer.echo(..., err=True)` or `print(..., file=sys.stderr)`. Don't move status output to stdout. The inbox TUI is the exception (it owns the terminal); its diagnostic logging is redirected to a file.

## Conventions worth knowing

- Type hints everywhere; pydantic v2 for all data models in `triage_cli/models.py`.
- No `print` outside `cli.py`, `render.py`, `pipeline.py`, and `watcher.py` (the latter two only for stderr status). Library modules return or raise.
- Module size budget ~150 lines; if you're growing one past that, the split usually wants to live in a sibling module, not a sub-package. (`investigation.py` and `inbox/app.py` are deliberate exceptions.)
- Internal Zendesk comments **are** sent to the LLM. v1 is terminal-only so this is acceptable; if anything ever posts back to Zendesk, this assumption must be revisited.
- `ruff` ruleset: `E,F,W,I,B,UP,SIM`, line length 100, `target-version = py311`. Keep edits compatible.
- TUI deps (`rich`, `textual`) are runtime-required; don't gate them behind extras.

## Where things live

| What | Where |
| --- | --- |
| LLM system prompt | `triage_cli/llm.py` (`TRIAGE_SYSTEM_PROMPT`) |
| Anchor extraction prompt | `triage_cli/llm.py` (`ANCHOR_EXTRACTION_PROMPT`) |
| Pipeline orchestration | `triage_cli/pipeline.py` (`investigate_one`) |
| Reporter protocol + implementations | `triage_cli/pipeline.py` (`Reporter`, `StderrReporter`, `SilentReporter`) |
| Investigation-progress TUI | `triage_cli/tui/` (`app.py`, `widgets.py`, `reporter.py`) |
| Investigation session + evidence | `triage_cli/investigation.py` |
| Memory layer (MEMORY.md + SQLite FTS5) | `triage_cli/memory.py`, `MEMORY.md`, `data/memory.db` |
| LLM provider abstraction | `triage_cli/providers/` (`base.py`, `unleash.py`, `claude.py`, `openai.py`) |
| Inbox TUI | `triage_cli/inbox/` (`app.py`, `widgets.py`, `clipboard.py`, `hydrate.py`) |
| Site lookup logic | `triage_cli/extract.py` (`lookup_site`) |
| Anchor priority | `triage_cli/extract.py` (`resolve_anchor`) |
| Datadog query construction | `triage_cli/datadog.py` (`get_logs`) |
| Watcher loop + state | `triage_cli/watcher.py` |
| Read-only certification | `scripts/certify_readonly_my_queue.py` |
| Operator runbooks | `docs/runbooks/` (01–08) |
| Quick reference | `docs/CHEATSHEET.md` |
| Completed feature specs/plans | `docs/superpowers/` |
