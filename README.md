# triage-cli

## What this is

A local CLI for Axon network engineers working the Carbyne APEX NG911/E911 platform. Give it a Zendesk ticket (URL or numeric ID) and it pulls the ticket and its full comment thread, resolves which APEX site it belongs to via a local CNC inventory, queries Datadog for logs in a window around the incident anchor, hands the bundle to Claude, and prints a four-section markdown triage note to your terminal. Single-shot, terminal-only, no posting back, no daemons.

## Prerequisites

- **Claude Code CLI installed and authenticated.** The `claude` command must work in your shell. The Claude Agent SDK piggybacks on Claude Code's existing OAuth session, so there is no API key to provision. If you have not run `claude` interactively at least once, do that first.
- **Python 3.11+** (the package is pinned to `>=3.11` in `pyproject.toml`).
- **Zendesk credentials** with read scope on tickets: an agent email plus an API token.
- **Datadog credentials**: an API key and an APP key with permission to read logs.

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

After install, `triage-cli --help` should list the `triage` and `build-map` subcommands.

## Configuration

Copy the example env file and fill in the credentials:

```bash
cp .env.example .env
```

| Variable | Purpose |
| --- | --- |
| `ZENDESK_SUBDOMAIN` | Your Zendesk subdomain (the `<sub>` in `<sub>.zendesk.com`). |
| `ZENDESK_EMAIL` | Agent email used for Basic auth. |
| `ZENDESK_API_TOKEN` | Zendesk API token. The client appends `/token` to the email automatically; do not append it yourself. |
| `DD_API_KEY` | Datadog API key. |
| `DD_APP_KEY` | Datadog application key. |
| `DD_SITE` | Datadog site host. Leave at default `datadoghq.com` unless you are on a non-US tenant. |
| `DD_CALL_CENTER_TAG` | Datadog tag key for the call-center filter. Leave at default `@log.machineData.callCenterName`. |
| `DD_STATION_TAG` | Reserved for v2 station-level filtering. Leave at default; v1 does not use it. |
| `ANTHROPIC_MODEL` | Model identifier passed to the Agent SDK. Default `claude-sonnet-4-6`. |

`ANTHROPIC_API_KEY` is intentionally absent. The Claude Agent SDK inherits Claude Code's auth.

## Building the site map

The site map at `data/cnc-map.json` is the lookup table from Zendesk requester orgs to APEX `site_name` values (the Datadog filter key) and CNC UUIDs. It is generated from the markdown inventory at `apex-cnc-inventory.md` by `scripts/build_cnc_map.py`.

To rebuild it:

```bash
triage-cli build-map
```

This rewrites `data/cnc-map.json` and `data/cnc-map-gaps.md` (the latter records inventory rows missing a CNC UUID or `site_name` so they can be filled in later). When the upstream Confluence inventory changes, refresh `apex-cnc-inventory.md` out-of-band — re-run the Claude Confluence connector against the source page, then re-run `build-map`. There is no `confluence.py` in this repo by design.

## Usage

### Basic happy path

```bash
triage-cli triage 12345
triage-cli triage https://<sub>.zendesk.com/agent/tickets/12345
```

The CLI accepts either a raw ticket ID or a full Zendesk URL. The output is a four-section markdown note printed to stdout.

### Save the note to disk

```bash
triage-cli triage 12345 --save
```

Also writes the rendered note to `./triage-notes/<ticket-id>-<timestamp>.md`. Stdout still shows the note.

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

## Output format

The triage note is plain markdown with four fixed sections, in this order:

```markdown
## Summary
Two sentences describing what the ticket reports. No speculation.

## Log signals
What the logs in the window show — error counts, recurring messages, timing
relative to the anchor. If logs are empty or routine, says so plainly.

## Likely cause (inference)
Best guess at cause, marked as inference. If the logs do not support a cause,
says "Insufficient log evidence to infer cause" rather than guessing.

## Suggested first action
One concrete next step ("check X" / "verify Y") for the engineer.
```

The exact wording of the system prompt that produces this lives in `triage_cli/llm.py` (`TRIAGE_SYSTEM_PROMPT`).

## Known limitations

These are the v1 boundary; do not assume any of them have been addressed:

- Site map is manually curated; refreshing the underlying Confluence inventory is an out-of-band step.
- No station-level log filtering. Only call-center level via `@log.machineData.callCenterName`. The `DD_STATION_TAG` env var is reserved for v2.
- No handling of ticket updates after the first run; single-shot only. There is no `agent-triaged` tag and no idempotency.
- Internal Zendesk comments are sent to Claude. Output is terminal-only for v1, but be aware before adding any "post back to Zendesk" feature — internal comments would leak into anything posted publicly.
- No retries on transient API failures; if Datadog or Zendesk hiccups, re-run the command.
- Single-user, local execution. No scheduling, no watcher, no shared state.

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
│   ├── cli.py                  # typer app: triage and build-map subcommands
│   ├── zendesk.py              # ticket + comment fetch (httpx)
│   ├── datadog.py              # log query (datadog-api-client)
│   ├── extract.py              # ticket ID parsing, site lookup, window/anchor
│   ├── llm.py                  # Claude Agent SDK calls + system prompts
│   ├── render.py               # markdown print + --save handling
│   └── models.py               # pydantic models
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
| Triage system prompt | `triage_cli/llm.py` (`TRIAGE_SYSTEM_PROMPT`) |
| Anchor extraction prompt | `triage_cli/llm.py` (`ANCHOR_EXTRACTION_PROMPT`) |
| Pipeline flow (numbered steps) | `triage_cli/cli.py` `triage()` |
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
