# triage-cli

## What this is

A local CLI for Axon network engineers working the Carbyne APEX NG911/E911 platform. It is built around two first-class workflows:

1. **Guided Investigation** (`triage-cli investigate <ticket>`) — the daily-use path. Fetches a Zendesk ticket, walks you through ingesting evidence (attachments, local log files or directories, pasted text), normalizes everything into a chronological timeline, and asks Claude for an assessment, likely root cause, unknowns, suggested next steps, and a Zendesk-ready internal note.

2. **Automated Watcher** (`triage-cli watch --view <id>`) — a polling loop that watches a Zendesk view, runs the legacy fast-path triage on new or updated tickets, and saves markdown + JSON artifacts to `./triage-notes/`. The accompanying `triage-cli inbox` TUI is the live review surface for those artifacts.

Datadog is **optional evidence enrichment**, not a required part of the core flow. The guided flow is fully usable without Datadog credentials. The legacy one-shot `triage-cli triage <ticket>` command is preserved for scripting and the watcher's reuse; it still queries Datadog by default unless `--no-logs` is passed.

Terminal-only, no posting back to Zendesk, single-user.

## Prerequisites

- **Claude Code CLI installed and authenticated.** The `claude` command must work in your shell. The Claude Agent SDK piggybacks on Claude Code's existing OAuth session, so there is no API key to provision. If you have not run `claude` interactively at least once, do that first.
- **Python 3.11+** (the package is pinned to `>=3.11` in `pyproject.toml`).
- **Zendesk credentials** with read scope on tickets: an agent email plus an API token.
- **Datadog credentials** (optional): an API key and an APP key with permission to read logs. Required only for the `triage` fast-path with Datadog enrichment, the `watch` loop with logs, or future Datadog evidence inside a guided investigation.

## Install

```bash
git clone <repo-url> && cd triage-cli
python3.11 -m venv .venv
source .venv/bin/activate
pip install -e .
```

`uv` works too if you prefer it:

```bash
uv pip install -e .
```

After install, `triage-cli --help` should list the `investigate`, `triage`, `watch`, `inbox`, and `build-map` subcommands.

## Configuration

Copy the example env file and fill in the credentials:

```bash
cp .env.example .env
```

| Variable | Required for | Purpose |
| --- | --- | --- |
| `ZENDESK_SUBDOMAIN` | always | Your Zendesk subdomain (the `<sub>` in `<sub>.zendesk.com`). |
| `ZENDESK_EMAIL` | always | Agent email used for Basic auth. |
| `ZENDESK_API_TOKEN` | always | Zendesk API token. The client appends `/token` to the email automatically; do not append it yourself. |
| `DD_API_KEY` | Datadog enrichment | Datadog API key. |
| `DD_APP_KEY` | Datadog enrichment | Datadog application key. |
| `DD_SITE` | Datadog enrichment | Datadog site host. Leave at default `datadoghq.com` unless you are on a non-US tenant. |
| `DD_CALL_CENTER_TAG` | Datadog enrichment | Datadog tag key for the call-center filter. Leave at default `@log.machineData.callCenterName`. |
| `DD_STATION_TAG` | reserved | Reserved for v2 station-level filtering. Leave at default; v1 does not use it. |
| `ANTHROPIC_MODEL` | optional | Model identifier passed to the Agent SDK. Default `claude-sonnet-4-6`. |

The Datadog variables are only consulted when:

- you run `triage-cli triage <id>` without `--no-logs`,
- you run `triage-cli watch` without `--no-logs`, or
- (future) you pick the Datadog evidence option from the guided investigation menu.

Guided investigations work end-to-end with only the Zendesk variables set.

`ANTHROPIC_API_KEY` is intentionally absent. The Claude Agent SDK inherits Claude Code's auth.

## Building the site map

The site map at `data/cnc-map.json` is the lookup table from Zendesk requester orgs to APEX `site_name` values (the Datadog filter key) and CNC UUIDs. It is generated from the markdown inventory at `apex-cnc-inventory.md` by `scripts/build_cnc_map.py`.

To rebuild it:

```bash
triage-cli build-map
```

This rewrites `data/cnc-map.json` and `data/cnc-map-gaps.md` (the latter records inventory rows missing a CNC UUID or `site_name` so they can be filled in later). When the upstream Confluence inventory changes, refresh `apex-cnc-inventory.md` out-of-band — re-run the Claude Confluence connector against the source page, then re-run `build-map`. There is no `confluence.py` in this repo by design.

## Usage

### Guided Investigation (primary workflow)

```bash
triage-cli investigate 12345
triage-cli investigate https://<sub>.zendesk.com/agent/tickets/12345
```

`investigate` is the daily-use path for engineers actively working a ticket. It:

1. Fetches the ticket and its full comment thread.
2. Lists any Zendesk attachments it found (metadata only — see "Attachments" below).
3. Prints a short summary, then loops a menu prompting you for evidence:

   ```
   Add evidence:
     [a] Ingest Zendesk attachment metadata
     [f] Add local file
     [d] Add local directory
     [p] Paste log text
     [s] Proceed to assessment
   ```

4. Parses each ingested source into chronological `TimelineEvent`s. ISO-8601 prefixed lines and JSON-line logs are recognized automatically; unrecognized lines are counted but not surfaced.
5. Builds one merged timeline across all sources (ticket created, comments, parsed log lines).
6. Sends the manifest + timeline to Claude and renders a `TriageReport` to your terminal.
7. Saves markdown + JSON to `./triage-notes/<id>-<timestamp>.{md,json}` (use `--no-save` to skip).

`investigate` does not require Datadog credentials, a resolved site, or any flags beyond the ticket id.

#### Attachments

Zendesk attachment ingestion is **metadata-only** in this release. Picking `[a]` records each attachment's filename, size, content type, and URL on the session, and surfaces that to the LLM as evidence-source manifest entries marked `[metadata only]`. Actual binary download and parsing (PDFs, screenshots, log archives) is a follow-up that needs an explicit per-attachment opt-in step — call recordings and CAD exports are a different privacy class than the comment text we already send.

### Fast path (one-shot, non-interactive)

```bash
triage-cli triage 12345
triage-cli triage https://<sub>.zendesk.com/agent/tickets/12345
```

A non-interactive transformation: ticket → site lookup → Datadog query → Claude → markdown to stdout. Use this for scripting, CI, and the watcher's internal reuse — not for active investigation. Datadog is queried by default; pass `--no-logs` for ticket-content-only triage.

### Save the note to disk

```bash
triage-cli triage 12345 --save
```

Also writes the rendered note to `./triage-notes/<ticket-id>-<timestamp>.md` and `.json`. Stdout still shows the note. (`investigate` saves both by default; pass `--no-save` to skip.)

### Verbose pipeline trace

```bash
triage-cli triage 12345 --verbose
```

Verbose output goes to stderr (so stdout stays clean for piping) and shows: ticket fetched, site resolution strategy that won, anchor source (`flag` / `extracted` / `created_at`), Datadog query parameters, log line count, and any anchor-extraction fallbacks.

### Override the anchor timestamp

```bash
triage-cli triage 12345 --at 2026-05-06T14:32:00Z
triage-cli triage 12345 --at "2026-05-06T14:32-07:00"
```

Use this when the ticket was filed well after the incident (the LLM-extracted anchor is best-effort, and `created_at` may be too late). `--at` is the highest-priority anchor source. Accepts ISO 8601 with offset, including a trailing `Z`.

### Override the site

```bash
triage-cli triage 12345 --site us-nv-nvdps-apex
triage-cli triage 12345 --cnc de9ee414-da5a-471d-bac2-10643190da0b
```

`--site` skips the lookup entirely and uses the value as-is in the Datadog filter. `--cnc` looks the entry up by UUID in `data/cnc-map.json` and uses its `site_name`. Use `--site` when the requester org or ticket text does not match any inventory entry.

### Skip Datadog

```bash
triage-cli triage 12345 --no-logs
```

Runs the LLM call on the ticket content alone. Useful when iterating on the system prompt without burning Datadog query quota, and when the site cannot be resolved but you still want a ticket-only summary.

### Adjust log levels

```bash
triage-cli triage 12345 --levels error,warn,info
```

Default is `error,warn`. Valid values: `error`, `warn`, `info`, `debug`. The flag accepts a comma-separated list and is validated up-front.

### Adjust the log window

```bash
triage-cli triage 12345 --window-minutes 60
```

Default window is 30 minutes either side of the resolved anchor. The result count is capped at 200 lines regardless; if Datadog returns more, the bundle is marked truncated and the rendered note flags it.

### Non-interactive mode

```bash
triage-cli triage 12345 --no-interactive
```

If site resolution fails, the CLI normally prompts for a `site_name`. With `--no-interactive` it aborts instead — use this in any shell pipeline or scheduled context.

### Rebuild the site map

```bash
triage-cli build-map
```

Runs `scripts/build_cnc_map.py` and prints a summary of entries written and gap entries logged.

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

## Output format

Both commands produce a `TriageReport` rendered as markdown to stdout (Rich-formatted to a TTY). `investigate` adds a `Summary` and `Correlation` section that the fast-path `triage` omits.

```markdown
# Triage Report — ZD-12345

**Confidence:** medium · **Sources:** zendesk, local_file:syslog.log(312)

## Summary               (investigate only)
2-4 sentences summarizing the ticket and the evidence.

## Finding
One or two sentences. Likely cause.

## Evidence
- timestamps and one-line excerpts the LLM relied on.

## Correlation           (investigate only)
- Bullets describing where signals line up.

## Next Checks
- Concrete verification steps.

## Unknowns
- What the LLM couldn't determine.

## Suggested Internal Note
Paste-ready Zendesk internal note.
```

System prompts:

- `triage_cli/llm.py` `TRIAGE_SYSTEM_PROMPT` — fast-path `triage` command.
- `triage_cli/llm.py` `ASSESSMENT_SYSTEM_PROMPT` — guided `investigate` command.

## Known limitations

These are the v1 boundary; do not assume any of them have been addressed:

- Site map is manually curated; refreshing the underlying Confluence inventory is an out-of-band step.
- No station-level log filtering. Only call-center level via `@log.machineData.callCenterName`. The `DD_STATION_TAG` env var is reserved for v2.
- Internal Zendesk comments are sent to Claude. Output is terminal-only for v1, but be aware before adding any "post back to Zendesk" feature — internal comments would leak into anything posted publicly.
- No retries on transient API failures; if Datadog or Zendesk hiccups, re-run the command.
- Single-user, local execution. No scheduling, no shared state. (`watch` mode provides local single-user polling without external scheduling.)

## Project layout

```
triage-cli/
├── pyproject.toml
├── README.md
├── HANDOFF.md                  # operative spec
├── CONVERSATION.md             # planning transcript
├── apex-cnc-inventory.md       # source of truth for the CNC map
├── .env.example
├── triage_cli/
│   ├── cli.py                  # typer app: investigate, triage, watch, inbox, build-map
│   ├── zendesk.py              # ticket + comment + attachment-metadata fetch (httpx)
│   ├── datadog.py              # log query (datadog-api-client)
│   ├── extract.py              # ticket ID parsing, site lookup, window/anchor
│   ├── llm.py                  # Claude Agent SDK calls + system prompts (triage + assess)
│   ├── models.py               # pydantic models (Ticket, TriageReport, ...)
│   ├── timeline.py             # TimelineEvent + ISO/JSON line parsing
│   ├── evidence.py             # EvidenceSource + ingestion helpers
│   ├── investigation.py        # InvestigationSession + assessment orchestration
│   ├── pipeline.py             # triage_one single-ticket orchestration (fast path)
│   ├── render.py               # markdown print + --save handling
│   ├── inbox/                  # Textual TUI: live review queue for the watcher
│   └── watcher.py              # watch command: poll loop, state, backfill
├── data/
│   ├── cnc-map.json            # generated; do not hand-edit
│   └── cnc-map-gaps.md         # generated; rows without CNC/site_name
├── scripts/
│   └── build_cnc_map.py        # parses apex-cnc-inventory.md
└── tests/
    ├── fixtures/
    └── test_extract.py
```

## Where to find things

| What | Where |
| --- | --- |
| Guided investigation flow | `triage_cli/cli.py` `investigate()` |
| Investigation orchestration | `triage_cli/investigation.py` (`InvestigationSession`, `run_assessment`) |
| Evidence ingestion helpers | `triage_cli/evidence.py` |
| Timeline normalization | `triage_cli/timeline.py` |
| Assessment system prompt | `triage_cli/llm.py` (`ASSESSMENT_SYSTEM_PROMPT`) |
| Triage system prompt (fast path) | `triage_cli/llm.py` (`TRIAGE_SYSTEM_PROMPT`) |
| Anchor extraction prompt | `triage_cli/llm.py` (`ANCHOR_EXTRACTION_PROMPT`) |
| Fast-path pipeline flow | `triage_cli/cli.py` `triage()` |
| Site lookup logic | `triage_cli/extract.py` (`lookup_site`, `load_site_map`) |
| Anchor resolution priority | `triage_cli/extract.py` (`resolve_anchor`) |
| Site map source | `apex-cnc-inventory.md` -> `scripts/build_cnc_map.py` -> `data/cnc-map.json` |
| Datadog query construction | `triage_cli/datadog.py` |
| Zendesk client | `triage_cli/zendesk.py` |

## Troubleshooting

**`ImportError: claude_agent_sdk`**
The package was not installed (run `pip install -e .` again from the repo root with your venv active), or your venv is not actually activated. The SDK is a runtime dependency declared in `pyproject.toml`.

**`Claude Agent SDK call failed` / `extracted_dt None and SDK error in --verbose`**
The SDK could not reach Claude Code's session. Run `claude` once interactively to confirm the CLI is installed and your OAuth session is valid. The Agent SDK does not read `ANTHROPIC_API_KEY` — Claude Code's auth is the only path.

**Zendesk auth failed (401 / 403)**
Check that `ZENDESK_API_TOKEN` is the API token (not your password) and that `ZENDESK_EMAIL` is the agent email associated with the token. The client appends `/token` to the email when forming Basic auth — do not pre-append it in `.env`. Also confirm the token has read scope on tickets.

**`site_name '<X>' contains characters that are unsafe`**
The Datadog client validates `site_name` before injecting it into the query string. Either fix the offending entry in `apex-cnc-inventory.md` and re-run `build-map`, or pass a clean value via `--site`. Site names should match the lowercase-with-hyphens convention (e.g. `us-nv-nvdps-apex`).

**Site cannot be resolved and you don't want the prompt**
Pass `--site <site_name>` (or `--cnc <uuid>`) to bypass lookup, or `--no-interactive` to abort instead of prompting.
