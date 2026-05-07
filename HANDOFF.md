# triage-cli: Build Handoff (Revised)

You are picking up a planning conversation that is now ready for implementation. The full transcript is in `CONVERSATION.md` for context. This document is the operative spec; if it conflicts with the transcript, this wins.

This revision supersedes the original handoff. Material changes from the prior version are listed at the bottom under "Changes from prior handoff."

## What this is

A Python CLI that takes a Zendesk ticket (URL or ID), pulls the ticket content, looks up the corresponding APEX site from a local JSON map, queries Datadog for logs in a relevant time window, sends the bundle to Claude, and prints a structured triage note to the terminal.

Single user, local execution, terminal output only. No scheduling, no posting, no persistence beyond the site map.

## Stack

- Python 3.11+
- `typer` for the CLI
- `httpx` for HTTP (Zendesk client)
- `pydantic` v2 for models
- `claude-agent-sdk` for the LLM call (NOT the `anthropic` SDK — see "LLM access" below)
- `datadog-api-client` for Datadog
- `python-dotenv` for env loading
- `pytest` for tests
- `ruff` for linting

Use `uv` for dependency management if convenient; otherwise standard `pyproject.toml` with `pip`.

## Project structure to create

```
triage-cli/
├── pyproject.toml
├── README.md
├── HANDOFF.md                  (this file — leave in place)
├── CONVERSATION.md             (planning transcript — leave in place)
├── apex-cnc-inventory.md       (source for the site map — leave in place)
├── .env.example
├── .gitignore
├── triage_cli/
│   ├── __init__.py
│   ├── cli.py
│   ├── zendesk.py
│   ├── datadog.py
│   ├── extract.py
│   ├── llm.py
│   ├── render.py
│   └── models.py
├── data/
│   ├── cnc-map.json
│   └── cnc-map-gaps.md
├── scripts/
│   └── build_cnc_map.py
└── tests/
    ├── __init__.py
    ├── fixtures/
    │   ├── sample_ticket.json
    │   └── sample_logs.json
    └── test_extract.py
```

No `confluence.py`. The CNC map is built from `apex-cnc-inventory.md` via `scripts/build_cnc_map.py` and committed to `data/cnc-map.json`. Refreshing the inventory is an out-of-band step (re-run the Confluence connector against `apex-cnc-inventory.md`, then re-run `build_cnc_map.py`); document this in the README.

## CLI shape

Two subcommands:

```
triage-cli triage <ticket-id-or-url>      # the main flow
triage-cli build-map                       # rebuild data/cnc-map.json from apex-cnc-inventory.md
```

Optional flags on `triage`:

- `--save` — also write the rendered note to `./triage-notes/<ticket-id>-<timestamp>.md`
- `--verbose` — show intermediate steps (ticket fetch, site resolved, anchor source, log count, etc.)
- `--no-logs` — skip Datadog, run on ticket content only (useful for iterating on the prompt)
- `--window-minutes N` — override the default Datadog window (default: 30 minutes either side of the resolved anchor)
- `--at <iso8601>` — explicit anchor timestamp override (highest priority)
- `--cnc <uuid>` — explicit CNC override (looks up the entry by UUID)
- `--site <site_name>` — explicit site_name override (used directly; bypasses lookup entirely)
- `--levels <comma-separated>` — log levels to include (default: `error,warn`; valid: `error,warn,info,debug`)
- `--no-interactive` — abort instead of prompting when site can't be resolved

Accept either a raw ticket ID (numeric) or a full Zendesk URL on `triage`; parse the ID out of the URL.

## Pipeline

```
[1] Parse ticket ID from input (URL or raw ID)
[2] Fetch ticket from Zendesk: subject, description, requester, org, tags, created_at, full comment thread (public + internal)
[3] Resolve site:
      - --site flag → use directly
      - --cnc flag → lookup by CNC UUID in cnc-map.json
      - friendly_name match against requester organization (case-insensitive exact)
      - site_name substring match in subject/body
      - friendly_name substring match in subject/body
      - Interactive prompt for site_name (unless --no-interactive, then abort)
[4] Resolve anchor timestamp:
      - --at flag → use directly
      - LLM extraction call against ticket subject + description + comments → ISO timestamp or null
      - Fall back to ticket.created_at
      - Log which source won when --verbose
[5] Query Datadog for logs:
      - Filter: <DD_CALL_CENTER_TAG>:<site_name> status:(<levels>)
      - Window: anchor ± --window-minutes (default 30)
      - Cap result count at 200 lines; note truncation in output
[6] Build LLM input bundle:
      - Ticket: subject, description, full comment thread (public + internal), tags, requester org
      - Logs: timestamp + level + message, in chronological order
      - Customer context: friendly name, site name, CNC UUID
[7] Call Claude with the system prompt below (via Claude Agent SDK)
[8] Render the response as markdown to stdout
[9] If --save, also write to ./triage-notes/<ticket-id>-<timestamp>.md
```

## Site map format

`data/cnc-map.json` is a list of objects:

```json
[
  {
    "friendly_name": "Nevada Department of Public Safety",
    "site_name": "us-nv-nvdps-apex",
    "cnc": "de9ee414-da5a-471d-bac2-10643190da0b"
  }
]
```

### Conversion rules (used by `scripts/build_cnc_map.py`)

Source: `apex-cnc-inventory.md` — two markdown tables.

1. **Per-site table is canonical.** Use those rows verbatim.
2. **Master table:** for rows whose CNC UUID is not already present in the per-site table, include with `friendly_name` set to the master-table display label and `site_name` derived from the display label *only if* the label looks like a site_name (lowercase with hyphens, starts with country code). Otherwise skip.
3. **Skip rows with blank/missing CNC UUIDs** (e.g., "(CNC field blank in source page)"). Append them to `data/cnc-map-gaps.md` with the row data so they can be filled in later.
4. **Fairfax Pine Ridge** has a documented copy/paste error in the inventory — preserve corrected `friendly_name: "Fairfax Pine Ridge"`, drop the parenthetical.
5. **Dedupe by CNC UUID.** Per-site entries always win over master-table entries.
6. **All entries in the JSON must have a non-null `site_name`** (this is the Datadog query key — entries without it are unusable).

Lookup function in `extract.py` should match in this order:

1. Exact `friendly_name` match (case-insensitive) against Zendesk requester organization
2. Substring match of `site_name` in ticket subject or body
3. Substring match of `friendly_name` in ticket subject or body

Return the first match. Log which strategy hit when `--verbose`.

## build-map subcommand

Runs `scripts/build_cnc_map.py` against `apex-cnc-inventory.md`. Output: rewrites `data/cnc-map.json` and `data/cnc-map-gaps.md`. Prints a summary (entries written, gap entries logged).

## LLM access — Claude Agent SDK (not the Anthropic SDK)

The user has an Axon enterprise OAuth seat for Claude but no provisioned API key. The Claude Agent SDK spawns Claude Code under the hood and inherits the user's existing auth — no `ANTHROPIC_API_KEY` required.

Pattern for `llm.py`:

```python
from claude_agent_sdk import query, ClaudeAgentOptions
import os

MODEL = os.getenv("ANTHROPIC_MODEL", "claude-sonnet-4-6")

async def triage(bundle: TriageBundle) -> str:
    options = ClaudeAgentOptions(
        system_prompt=TRIAGE_SYSTEM_PROMPT,
        model=MODEL,
    )
    chunks: list[str] = []
    async for message in query(prompt=bundle.as_user_message(), options=options):
        # collect AssistantMessage text blocks
        ...
    return "".join(chunks)


async def extract_anchor(ticket: Ticket) -> datetime | None:
    """Best-effort timestamp extraction from ticket content. Returns None if no clear timestamp."""
    options = ClaudeAgentOptions(
        system_prompt=ANCHOR_EXTRACTION_PROMPT,  # asks for JSON {"timestamp": "<iso>" | null}
        model=MODEL,
    )
    # parse JSON response, return datetime or None
    ...
```

Wrap the async calls in `asyncio.run(...)` from `cli.py`. Do not introduce a provider abstraction layer; if the model swaps later (e.g., to Codex), it's a `llm.py` rewrite.

Prerequisites: Claude Code CLI installed and authenticated (`claude` command works in the user's shell). Document this in the README.

## Datadog query

Filter shape:

```
<DD_CALL_CENTER_TAG>:<site_name> status:(<level1> OR <level2>)
```

Default tag key: `@log.machineData.callCenterName`. Configurable via `DD_CALL_CENTER_TAG` env var.

Station-level filter (`@log.machineData.name`) is **not** used in v1. Bake the env var (`DD_STATION_TAG`) in for v2 readiness; leave the v1 query at site level only.

Level field: Datadog's standard `status:` (top-level, not an attribute). Confirm in the Datadog UI before shipping if unsure.

Window: `[anchor - window_minutes, anchor + window_minutes]` in UTC.

Cap result count at 200 lines. If Datadog returns more, take the first 200 chronologically and set `log_truncated=True` on the bundle. Render notes truncation in the Log signals section header (e.g., "Log signals (200 lines, truncated)").

## LLM prompt (system) — main triage call

```
You are a triage assistant for a Network Engineer working on the Carbyne APEX
NG911/E911 platform at Axon. You receive a Zendesk ticket and a window of
Datadog logs from the affected customer. Produce a structured triage note in
markdown with exactly these four sections, in this order:

## Summary
Two sentences. What the ticket reports. No speculation.

## Log signals
What the logs actually show in the window. Quote sparingly. Note error
counts, recurring messages, and timing relative to the anchor timestamp. If
logs are empty or all routine, say so plainly. Do not infer causes here.

## Likely cause (inference)
Your best guess at the cause, given the ticket and logs. Mark this section as
inference. If the logs do not support a cause, say "Insufficient log evidence
to infer cause" rather than guessing.

## Suggested first action
One concrete step the engineer should take first. Prefer "check X" or
"verify Y" over open-ended advice. If you cannot suggest a useful action,
say so.

Rules:
- Do not invent log lines, error codes, ticket IDs, or past incidents.
- Do not assign priority or confidence scores.
- Do not pad. Empty findings are valid findings.
- If sections 2 and 3 disagree, that is signal; do not paper over it.
```

## LLM prompt (system) — anchor extraction call

```
You extract the most likely incident timestamp from a Zendesk ticket. Read
the subject, description, and comments. Return JSON with a single field:

{"timestamp": "<ISO 8601 in UTC>" or null}

Return null if there is no clear timestamp in the content. Do not guess. A
generic "this morning" with no date is null. An explicit "2026-05-06 14:32 PT"
is a timestamp. When in doubt, return null.
```

Parse the response as JSON; on parse error, return None and let the caller fall back to `created_at`.

## models.py

Pydantic models for:

- `Ticket` (id, subject, description, requester_org, tags, created_at, comments)
- `Comment` (author, body, created_at, is_public)
- `LogLine` (timestamp, level, message, attributes)
- `SiteEntry` (friendly_name, site_name, cnc) — represents one entry in `cnc-map.json`
- `TriageBundle` (ticket, site_entry, log_lines, log_truncated, anchor, anchor_source)
- `TriageNote` (raw markdown string from the LLM; no schema enforcement on the output for v1)

`anchor_source` is an enum: `flag | extracted | created_at`. Surfaced in `--verbose` output.

## Environment variables

`.env.example`:

```
ZENDESK_SUBDOMAIN=
ZENDESK_EMAIL=
ZENDESK_API_TOKEN=
DD_API_KEY=
DD_APP_KEY=
DD_SITE=datadoghq.com
DD_CALL_CENTER_TAG=@log.machineData.callCenterName
DD_STATION_TAG=@log.machineData.name
ANTHROPIC_MODEL=claude-sonnet-4-6
```

No `ANTHROPIC_API_KEY` — the Claude Agent SDK uses Claude Code's auth.

`.gitignore` must exclude `.env`, `triage-notes/`, `__pycache__/`, `.venv/`, `*.egg-info/`, and `.pytest_cache/`.

## Tests

For v1, only `test_extract.py` is required. Cover:

- Ticket ID extraction from URL forms (`https://<sub>.zendesk.com/agent/tickets/12345`, raw `12345`, etc.)
- Site lookup: friendly_name org match, site_name substring, friendly_name substring, no-match case
- Time window construction from an anchor and a window-minutes value
- Anchor resolution priority (--at flag > extracted > created_at), with the LLM call mocked

Stub Zendesk and Datadog clients with the fixture files. No live API calls in tests. Do not test the Agent SDK call directly in v1.

## README

Should cover:

1. What the tool does (one paragraph)
2. **Prerequisites** — Claude Code CLI installed and authenticated, Python 3.11+, valid Zendesk and Datadog credentials
3. Install (`pip install -e .` or `uv pip install -e .`)
4. Configuration (`.env` setup, building `data/cnc-map.json` via `triage-cli build-map`)
5. Refreshing the site map — re-run the Confluence connector against `apex-cnc-inventory.md` (out-of-band, manual), then `triage-cli build-map`
6. Usage examples for both subcommands, including the new flags
7. Known limitations:
   - Site map manually compiled from Confluence
   - No station-level log filtering
   - No handling of ticket updates after first run; the `agent-triaged` tag concept is not implemented (single-shot only)
   - Single-shot only; no scheduling or watching
   - Internal Zendesk comments are sent to Claude — terminal-only output for v1, but be aware if this graduates to anything that posts back

## Build order

Build in this order so each layer is testable in isolation:

1. `pyproject.toml`, `.env.example`, `.gitignore`, package skeleton
2. `models.py`
3. `scripts/build_cnc_map.py` — parse `apex-cnc-inventory.md`, emit `data/cnc-map.json` and `data/cnc-map-gaps.md`. Run once to populate.
4. `extract.py` (ticket ID parsing, site lookup, window construction, anchor resolution priority) + tests
5. `zendesk.py` (one method: `get_ticket(ticket_id) -> Ticket`)
6. `datadog.py` (one method: `get_logs(site_name, levels, start, end) -> tuple[list[LogLine], bool]` — returns logs and truncated flag)
7. `llm.py` (two methods: `triage(bundle) -> str` and `extract_anchor(ticket) -> datetime | None`, both async)
8. `render.py` (markdown output, --save handling)
9. `cli.py` (wire everything; `triage` and `build-map` subcommands; `asyncio.run` for the async LLM calls)
10. README

## Explicit non-goals

Do not build any of the following, even if they seem like obvious extensions:

- Posting to Zendesk
- Scheduled execution / watcher / daemon mode
- Pattern matching against historical tickets or incidents
- Deduplication
- Station-level log filtering (the env var is reserved; the query is not)
- Re-triage on ticket updates (no `agent-triaged` tag handling)
- AWS Lambda packaging
- Any UI beyond the terminal
- Provider abstraction layer (Codex, OpenAI, etc.) — direct Agent SDK use only
- A `confluence.py` module of any kind

If you find yourself wanting to add one of these, stop and surface it to the user instead.

## Style

- Type hints everywhere
- Docstrings on every public function
- No print statements outside `cli.py` and `render.py`; everything else returns or raises
- Use `typer.echo` and `typer.secho` for CLI output, with `--verbose` controlling detail
- Keep individual modules under ~150 lines where possible
- Standard library `logging` for any debug instrumentation, off by default
- Async functions only where the Agent SDK requires them; everything else is synchronous

## When you are done

Print a summary of:
1. Files created
2. How to install and run a first invocation
3. Contents of `data/cnc-map-gaps.md` (so the user knows what's missing)
4. Any decisions you made that weren't specified above

Do not run the tool against live APIs to "verify it works." The user will do that.

---

## Changes from prior handoff

For traceability against `CONVERSATION.md`:

1. **LLM access switched from `anthropic` SDK to `claude-agent-sdk`.** User has enterprise OAuth, no API key. Agent SDK inherits Claude Code's auth. `ANTHROPIC_API_KEY` removed from `.env.example`.
2. **Datadog filter is by `site_name` (not CNC UUID).** Tag key: `@log.machineData.callCenterName`, configurable via `DD_CALL_CENTER_TAG`. CNC UUID stays in the map as metadata.
3. **`--at` flag added; LLM-based timestamp extraction added.** Anchor resolution: `--at → extracted → created_at`. The Datadog window is now anchored on the resolved anchor, not blindly on `created_at`.
4. **`--cnc` and `--site` flags added.** Plus `--no-interactive` for non-prompted aborts.
5. **`--levels` flag added (default `error,warn`).** Valid values: `error,warn,info,debug`.
6. **CNC map ships full inventory, not just Nevada example.** Conversion script (`scripts/build_cnc_map.py`) generates it from `apex-cnc-inventory.md`. New `build-map` subcommand replaces `refresh-cnc`.
7. **Inventory gap rows dropped from JSON, logged to `data/cnc-map-gaps.md`.** Entries without a `site_name` are unusable for Datadog and excluded.
8. **`confluence.py` module deleted from the spec.** README documents the manual refresh path instead.
9. **Internal Zendesk comments included in LLM bundle** for v1 (terminal-only; revisit if posting is added).
10. **Model pinned to `claude-sonnet-4-6`** in `.env.example`; code stays model-agnostic via `ANTHROPIC_MODEL` env var.
