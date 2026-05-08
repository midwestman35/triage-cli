# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A Python 3.11+ CLI that triages Zendesk tickets for the Carbyne APEX NG911/E911 platform. Three subcommands:

- `triage-cli triage <id-or-url>` — single-shot pipeline: fetch ticket → resolve site → query Datadog → call Claude → print markdown.
- `triage-cli watch --view <id>` — long-running poll loop over a Zendesk view, calling the same pipeline per ticket.
- `triage-cli build-map` — regenerates `data/cnc-map.json` from `apex-cnc-inventory.md`.

`README.md` is the user-facing spec; `HANDOFF.md` is the operative implementation spec (treat it as authoritative if it conflicts with the README).

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
```

Tests never hit live APIs — Zendesk, Datadog, and the Agent SDK are all stubbed/monkeypatched. Don't add network-touching tests.

## Architecture

### Pipeline ownership

The triage flow is intentionally split so a single orchestration function is shared by both subcommands:

- `cli.triage` (and `cli.watch`) — flag validation, env loading, ticket fetch, site map load, site resolution (including the interactive prompt), client lifetime management.
- `pipeline.triage_one` — anchor resolution → Datadog query → LLM call → returns markdown. **No I/O outside the injected clients.** Both `cli.triage` and `watcher.run_iteration` call this; keep new logic here, not duplicated in either caller.
- `render` — stdout printing and `--save` to `./triage-notes/<id>-<ts>.md`.

When extending the pipeline (e.g., new bundle inputs, anchor sources, log filters), add it inside `triage_one` so `watch` inherits the change for free.

### LLM access — Claude Agent SDK, not the Anthropic SDK

`triage_cli/llm.py` uses `claude-agent-sdk` (`query` + `ClaudeAgentOptions`). The Agent SDK spawns Claude Code under the hood and inherits the user's OAuth session — there is intentionally **no `ANTHROPIC_API_KEY`** in `.env.example` and the SDK does not read one. Prerequisite: the `claude` CLI must be installed and authenticated.

Do not "fix" this by switching to the `anthropic` HTTP SDK. The user has an enterprise OAuth seat with no provisioned API key; that path doesn't work for them. Model is read from `ANTHROPIC_MODEL` env (default `claude-sonnet-4-6`) — staying model-agnostic via env is the only abstraction; do not add a provider layer.

Two single-turn async calls live in `llm.py`:
- `triage(bundle)` — main markdown generation.
- `extract_anchor(ticket)` — best-effort timestamp extraction; returns `None` on any failure mode (invalid JSON, missing key, unparseable timestamp). Only Agent SDK transport errors raise.

Both are wrapped in `asyncio.run(...)` from `pipeline.triage_one`.

### Site map flow

`apex-cnc-inventory.md` (committed markdown tables) → `scripts/build_cnc_map.py` (invoked via `triage-cli build-map`) → `data/cnc-map.json` + `data/cnc-map-gaps.md`. Conversion rules (per-site table is canonical, master table fills gaps, blank-CNC rows go to gaps file, dedupe by CNC UUID, all retained entries must have a non-null `site_name`) are documented in `HANDOFF.md` — preserve them when editing `build_cnc_map.py`.

There is **no `confluence.py`** by design. Refreshing the inventory from Confluence is an out-of-band manual step. Do not add a Confluence module.

`extract.lookup_site` resolution priority: `--site` flag → `--cnc` flag → exact `friendly_name` match (case-insensitive) against `requester_org` → longest `site_name` substring in subject+description → longest `friendly_name` substring. The strategy name is returned for `--verbose` output.

### Anchor resolution

`extract.resolve_anchor` priority: `--at` flag (`AnchorSource.FLAG`) → LLM-extracted (`AnchorSource.EXTRACTED`) → `ticket.created_at` (`AnchorSource.CREATED_AT`). All datetimes are normalized to timezone-aware UTC inside the pipeline; naive inputs are treated as UTC (`extract._to_utc`, `datadog._ensure_aware`). Do not silently drop tzinfo when adding date logic.

### Datadog query

Site-level only: `<DD_CALL_CENTER_TAG>:<site_name> status:(<levels>)`. Window is `anchor ± window_minutes`, capped at 200 lines (`log_truncated=True` when at the cap). `site_name` is regex-validated (`^[a-zA-Z0-9._-]+$`) before query interpolation — do not loosen this. The `DD_STATION_TAG` env var is reserved for v2 station-level filtering and is read by no code today; leave it in `.env.example`.

### Watcher state

`data/watcher-state-<view-id>.json` has shape `{"version": 1, "triaged": {"<ticket-id>": "<iso updated_at>"}}`. `watcher.should_triage` is the pure decider (re-triage when `updated_at` advances; first-run silent backfill marks pre-cutoff tickets as seen with no note). State writes are atomic (tempfile + `os.replace`) and pruned to 1000 entries (`prune_state`) at the end of each iteration. Bumping `STATE_VERSION` requires a migration path, not a hard fail — current code raises on version mismatch, which is fine for a single-user tool but keep that in mind.

### Stdout vs stderr discipline

stdout is reserved for the rendered triage note so the output is pipeable. Everything else (verbose traces, spinners, watcher status lines, save-path notices) goes to stderr via `typer.echo(..., err=True)` or `print(..., file=sys.stderr)`. Don't move status output to stdout.

## Conventions worth knowing

- Type hints everywhere; pydantic v2 for all data models in `triage_cli/models.py`.
- No `print` outside `cli.py`, `render.py`, `pipeline.py`, and `watcher.py` (the latter two only for stderr status). Library modules return or raise.
- Module size budget ~150 lines; if you're growing one past that, the split usually wants to live in a sibling module, not a sub-package.
- Internal Zendesk comments **are** sent to the LLM. v1 is terminal-only so this is acceptable; if anything ever posts back to Zendesk, this assumption must be revisited.
- `ruff` ruleset: `E,F,W,I,B,UP,SIM`, line length 100, `target-version = py311`. Keep edits compatible.

## Where things live

| What | Where |
| --- | --- |
| Triage system prompt | `triage_cli/llm.py` (`TRIAGE_SYSTEM_PROMPT`) |
| Anchor extraction prompt | `triage_cli/llm.py` (`ANCHOR_EXTRACTION_PROMPT`) |
| Pipeline orchestration | `triage_cli/pipeline.py` (`triage_one`) |
| Site lookup logic | `triage_cli/extract.py` (`lookup_site`) |
| Anchor priority | `triage_cli/extract.py` (`resolve_anchor`) |
| Datadog query construction | `triage_cli/datadog.py` (`get_logs`) |
| Watcher loop + state | `triage_cli/watcher.py` |
| Operator runbooks | `docs/runbooks/` |
| Completed feature specs/plans | `docs/superpowers/` |
