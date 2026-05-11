# triage-cli

## What this is

A local CLI for Axon network engineers working the Carbyne APEX NG911/E911 platform. The primary workflow is Guided Investigation: give it a Zendesk ticket URL or ID, it fetches the ticket body, comments, and attachment metadata, optionally folds in local files or pasted logs, and generates a local markdown/JSON handoff draft. Nothing is posted back to Zendesk.

`triage-cli triage <ticket>` remains available as a fast one-shot summary/report path. `triage-cli watch --view <id>` remains the automated watcher for Zendesk views. Datadog is useful enrichment for `triage` and watcher mode, but it is not required for `investigate`.

## Prerequisites

- **Python 3.11+** (the package is pinned to `>=3.11` in `pyproject.toml`).
- **Zendesk credentials** with read scope on tickets: an agent email plus an API token.
- **LLM provider access** for `triage` and watcher reports. Production defaults to Unleash; Claude Code and OpenAI/Codex HTTP are explicit fallbacks. The guided `investigate` command does not require an LLM.
- **Datadog credentials** are optional enrichment for `triage` and watcher mode. `investigate` works without Datadog.

## Install

For first-time setup, use the interactive setup script. It creates the venv,
installs the package with dev dependencies, prompts for `.env`, builds the site
map, and resumes safely if interrupted:

```bash
python3.11 scripts/setup.py
```

Manual install remains available when you need to run the steps yourself:

```bash
git clone <repo-url> && cd triage-cli
python3.11 -m venv .venv
source .venv/bin/activate
python -m ensurepip --upgrade
python -m pip install --upgrade pip setuptools wheel
python -m pip install -e .
```

`uv` works too if you prefer it:

```bash
uv pip install -e .
```

After install, `triage-cli --help` should list the `investigate`, `triage`, `inbox`,
`watch`, `setup`, `doctor`, and `build-map` subcommands.

Use `triage-cli setup` to rerun or resume local setup after the console command
exists. Use `triage-cli doctor` to check local configuration, writable output
paths, and provider prerequisites.

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
| `DD_API_KEY` | Optional Datadog API key for `triage`/watch enrichment. |
| `DD_APP_KEY` | Optional Datadog application key for `triage`/watch enrichment. |
| `DD_SITE` | Datadog site host. Leave at default `datadoghq.com` unless you are on a non-US tenant. |
| `DD_CALL_CENTER_TAG` | Datadog tag key for the call-center filter. Leave at default `@log.machineData.callCenterName`. |
| `DD_STATION_TAG` | Reserved for v2 station-level filtering. Leave at default; v1 does not use it. |
| `LLM_PROVIDER` | LLM backend for `triage`/watcher calls. Defaults to `unleash`; supported values are `unleash`, `claude`, `openai`, and `codex` (`codex` is an alias for OpenAI Responses API). |
| `UNLEASH_API_KEY` | Unleash API key. Required when `LLM_PROVIDER=unleash`. |
| `UNLEASH_BASE_URL` | Unleash API base URL. Default `https://e-api.unleash.so`; private tenants usually use `https://<tenant>/e-api`. |
| `UNLEASH_ASSISTANT_ID` | Dedicated Unleash assistant ID for triage output. Required when `LLM_PROVIDER=unleash`. |
| `UNLEASH_ACCOUNT` | Optional impersonation account for Unleash impersonated API keys. Leave blank for non-impersonated keys. |
| `OPENAI_API_KEY` | OpenAI API key. Required when `LLM_PROVIDER=openai` or `LLM_PROVIDER=codex`. |
| `OPENAI_BASE_URL` | OpenAI-compatible Responses API base URL. Default `https://api.openai.com/v1`. |
| `OPENAI_MODEL` | Responses API model used by the OpenAI/Codex provider. Default `gpt-5.5`. |
| `ANTHROPIC_MODEL` | Claude model identifier used only when `LLM_PROVIDER=claude`. Default `claude-sonnet-4-6`. |

`ANTHROPIC_API_KEY` is intentionally absent. Claude fallback uses the local Claude Code OAuth session and requires the optional `claude` extra: `python -m pip install -e ".[claude]"`.

## Building the site map

The site map at `data/cnc-map.json` is the lookup table from Zendesk requester orgs to APEX `site_name` values (the Datadog filter key) and CNC UUIDs. It is generated from the markdown inventory at `apex-cnc-inventory.md` by `scripts/build_cnc_map.py`.

To rebuild it:

```bash
triage-cli build-map
```

This rewrites `data/cnc-map.json` and `data/cnc-map-gaps.md` (the latter records inventory rows missing a CNC UUID or `site_name` so they can be filled in later). When the upstream Confluence inventory changes, refresh `apex-cnc-inventory.md` out-of-band — re-run the Claude Confluence connector against the source page, then re-run `build-map`. There is no `confluence.py` in this repo by design.

## Usage

### Guided investigation

```bash
triage-cli investigate 12345
triage-cli investigate https://<sub>.zendesk.com/agent/tickets/12345
```

This is the primary daily-use path. It reads Zendesk ticket content, comments, and attachment metadata, then creates a local handoff draft with an evidence-first assessment. Attachment contents are not downloaded yet; metadata is recorded so the gap is visible.

Add local or pasted evidence in a testable, non-interactive way:

```bash
triage-cli investigate 12345 --file ./station.log --paste 'console=WARN audio dropped'
triage-cli investigate 12345 --save
triage-cli investigate 12345 --verbose
```

`--file` and `--paste LABEL=TEXT` may be repeated. `--save` writes paired markdown and JSON files under `./triage-notes/`. The generated note is local and paste-ready; the CLI does not write to Zendesk.

Evidence sources currently supported by Guided Investigation:

- Zendesk ticket body and metadata
- Zendesk comments
- Zendesk attachment metadata
- Local files
- Pasted logs or console excerpts

Datadog remains optional enrichment for `triage` and watcher mode; it is not used by `investigate`. Site map lookup, CNC resolution, and Claude are not required for `investigate`.

### Fast one-shot triage

```bash
triage-cli triage 12345
triage-cli triage https://<sub>.zendesk.com/agent/tickets/12345
```

The CLI accepts either a raw ticket ID or a full Zendesk URL. This path is optimized for a quick terminal report and can use site/CNC lookup, Datadog logs, and Claude.

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

### Skip Datadog for one-shot triage

```bash
triage-cli triage 12345 --no-logs
```

Runs the LLM call on the ticket content alone. Guided Investigation does not need this flag because Datadog is not part of its required path.

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

### Redaction

By default, caller PII (phone numbers, street addresses, GPS coords) is replaced with `<PHONE>` / `<ADDR>` / `<COORDS>` placeholders before any text is sent to Claude. Use `--no-redact` to disable for debugging.

```bash
triage-cli triage 12345 --no-redact
```

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

## One-Shot Output Format

The one-shot `triage` note is plain markdown with four fixed sections, in this order:

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
- Internal Zendesk comments are sent to the configured LLM provider. Caller PII is redacted by default (`--no-redact` disables this), but output is terminal-only for v1 — be aware before adding any "post back to Zendesk" feature.
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
│   ├── cli.py                  # typer app: investigate, triage, setup, doctor, watch, and build-map subcommands
│   ├── setup.py                # shared setup and doctor checks
│   ├── zendesk.py              # ticket + comment fetch (httpx)
│   ├── datadog.py              # log query (datadog-api-client)
│   ├── extract.py              # ticket ID parsing, site lookup, window/anchor
│   ├── investigation.py        # guided investigation session/evidence/report bridge
│   ├── llm.py                  # provider protocol, LLM calls, and system prompts
│   ├── models.py               # pydantic models
│   ├── pipeline.py             # triage_one single-ticket orchestration
│   ├── render.py               # markdown print + --save handling
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
| Triage system prompt | `triage_cli/llm.py` (`TRIAGE_SYSTEM_PROMPT`) |
| Anchor extraction prompt | `triage_cli/llm.py` (`ANCHOR_EXTRACTION_PROMPT`) |
| Pipeline flow (numbered steps) | `triage_cli/cli.py` `triage()` |
| Site lookup logic | `triage_cli/extract.py` (`lookup_site`, `load_site_map`) |
| Anchor resolution priority | `triage_cli/extract.py` (`resolve_anchor`) |
| Site map source | `apex-cnc-inventory.md` -> `scripts/build_cnc_map.py` -> `data/cnc-map.json` |
| Datadog query construction | `triage_cli/datadog.py` |
| Zendesk client | `triage_cli/zendesk.py` |

## Troubleshooting

**`UNLEASH_API_KEY must be set` / `UNLEASH_ASSISTANT_ID must be set`**
One-shot `triage` or watcher mode is using the production Unleash provider without the required Unleash credentials. Fill in `.env`, then re-run.

**`OPENAI_API_KEY must be set`**
`LLM_PROVIDER=openai` or `LLM_PROVIDER=codex` is selected without an OpenAI API key. Fill in `.env`, then re-run.

**`ImportError: claude_agent_sdk` / `claude-agent-sdk is not installed`**
Claude fallback was selected without installing the optional dependency. Install with `python -m pip install -e ".[claude]"`, confirm the venv is active, and verify the local `claude` CLI is authenticated.

**`Claude Agent SDK call failed` / `extracted_dt None and SDK error in --verbose`**
The SDK could not reach Claude Code's session while `LLM_PROVIDER=claude`. Run `claude` once interactively to confirm the CLI is installed and your OAuth session is valid. The Agent SDK does not read `ANTHROPIC_API_KEY`.

**Zendesk auth failed (401 / 403)**
Check that `ZENDESK_API_TOKEN` is the API token (not your password) and that `ZENDESK_EMAIL` is the agent email associated with the token. The client appends `/token` to the email when forming Basic auth — do not pre-append it in `.env`. Also confirm the token has read scope on tickets.

**`site_name '<X>' contains characters that are unsafe`**
The Datadog client validates `site_name` before injecting it into the query string. Either fix the offending entry in `apex-cnc-inventory.md` and re-run `build-map`, or pass a clean value via `--site`. Site names should match the lowercase-with-hyphens convention (e.g. `us-nv-nvdps-apex`).

**Site cannot be resolved and you don't want the prompt**
Pass `--site <site_name>` (or `--cnc <uuid>`) to bypass lookup, or `--no-interactive` to abort instead of prompting.
