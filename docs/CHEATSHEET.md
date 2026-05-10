# triage-cli cheatsheet

## What it is

A CLI that investigates and triages Zendesk tickets for the Carbyne APEX NG911/E911 platform. Guided Investigation is the primary workflow; the older one-shot and watcher paths remain available.

| Command | Purpose |
| --- | --- |
| `investigate <id>` | Guided investigation: fetch ticket/comments/attachment metadata, add local/pasted evidence, print or save a local handoff draft. |
| `triage <id>` | One-shot: fetch ticket → optional site/Datadog enrichment → call Claude → print a paste-ready report. |
| `inbox --view N` | Interactive Textual TUI: live list of view tickets on the left, selected report on the right. Use this at the keyboard. |
| `watch --view N` | Headless poll loop: forever poll a Zendesk view, save each new triage to `./triage-notes/`, print status to stderr. Use this in tmux/cron/systemd. |
| `setup` | Run or resume the interactive local setup flow after the console command is installed. |
| `build-map` | Regenerate `data/cnc-map.json` from `apex-cnc-inventory.md`. Run after editing the inventory. |

## First-time setup

```bash
# Fresh clone: bootstrap the venv, editable install, .env, site map, and CLI smoke test.
python3.11 scripts/setup.py

# After triage-cli is installed: rerun or repair the same setup flow.
triage-cli setup
```

## `investigate <id>` — guided investigation

```bash
triage-cli investigate 12345
triage-cli investigate 'https://acme.zendesk.com/agent/tickets/12345'
triage-cli investigate 12345 --file ./station.log
triage-cli investigate 12345 --paste 'console=WARN audio dropped'
triage-cli investigate 12345 --file ./station.log --paste 'dispatch=PTT failures' --save
triage-cli investigate 12345 --verbose
```

This path needs Zendesk only. It does not resolve CNC/site metadata, query Datadog, call Claude, or post notes back to Zendesk. Output is a local markdown handoff draft; `--save` writes paired `.md` and `.json` artifacts to `triage-notes/`.

## `triage <id>` — one-shot

```bash
triage-cli triage 12345                          # ID
triage-cli triage 'https://acme.zendesk.com/agent/tickets/12345'   # or URL
triage-cli triage 12345 --save                   # also write .md + .json to triage-notes/
triage-cli triage 12345 --verbose                # stderr trace + post-run confidence summary
triage-cli triage 12345 --no-logs                # skip Datadog (ticket content only)
triage-cli triage 12345 --window-minutes 60      # widen the Datadog window
triage-cli triage 12345 --at 2026-05-07T14:03:00Z   # override anchor timestamp
triage-cli triage 12345 --site us-co-aurora-apex    # bypass site lookup
triage-cli triage 12345 --cnc 921d7c53-e815-...     # use CNC UUID instead of name
triage-cli triage 12345 --levels error,warn,info    # broaden log levels
triage-cli triage 12345 | bat -l md              # pipe → raw markdown (TTY-only render is suppressed)
triage-cli triage 12345 --no-interactive         # abort if site can't be resolved (no prompt)
triage-cli triage 12345 --no-redact              # disable PII redaction (phone/addr/coords) for debugging
```

## `inbox --view N` — interactive TUI

```bash
triage-cli inbox --view 360123                   # default 60s poll, 0 backfill
triage-cli inbox --view 360123 --poll 30         # tighter polling (min 10)
triage-cli inbox --view 360123 --backfill 8h     # also triage tickets updated in the last 8h
triage-cli inbox --view 360123 --backfill inf    # everything in the view, ever
triage-cli inbox --view 360123 --window-minutes 30 --levels error,warn,info
triage-cli inbox --view 360123 --verbose         # WARNING+ goes to data/inbox-<view>.log at DEBUG
```

### Keys

| Key | Action |
| --- | --- |
| `↑` / `k` | Move cursor up |
| `↓` / `j` | Move cursor down |
| `d` | Cycle row density (comfortable ↔ compact) |
| `enter` | Move keyboard focus into the detail pane (so PgUp/PgDn scrolls the report) |
| `escape` | Return focus to the list |
| `r` | Force an immediate poll (interval keeps its phase) |
| `y` | Copy the selected report's `suggested_note` to the system clipboard |
| `o` | Open the ticket in your browser; URL also shown as a 10s toast (works for headless SSH) |
| `q` / `Ctrl-C` | Quit |

### Status icons

`✓` triaged · `→` triaging · `○` pending (in view but not yet triaged) · `✗` failed (will retry) · `◉` selected row.

## `watch --view N` — headless

```bash
triage-cli watch --view 360123                   # default 60s, 0 backfill, no log printing
triage-cli watch --view 360123 --interval 30
triage-cli watch --view 360123 --backfill 24h    # backfill the last day
triage-cli watch --view 360123 --print-notes     # also print each rendered note to stdout
triage-cli watch --view 360123 --no-logs         # skip Datadog for every ticket
triage-cli watch --view 360123 --state-file /tmp/state.json  # override state location
```

Use `tmux` / `nohup` / `systemd` to keep it running. The watcher saves `triage-notes/<id>-<ts>.{md,json}` for every ticket and writes its progress to `data/watcher-state-<view>.json`.

## `build-map`

```bash
triage-cli build-map        # regenerate data/cnc-map.json + data/cnc-map-gaps.md
```

Run after editing `apex-cnc-inventory.md`. `triage` and watcher site resolution depend on the JSON map; `investigate` does not.

## Where things live

| Path | What | Cleanup |
| --- | --- | --- |
| `triage-notes/<id>-<ts>.md` | Paste-ready markdown report | Manual; safe to delete old ones |
| `triage-notes/<id>-<ts>.json` | Structured `TriageReport`; inbox hydrates from this | Inbox hydration only looks at the last 24h |
| `data/cnc-map.json` | Site map (regenerated by `build-map`) | Don't edit by hand |
| `data/cnc-map-gaps.md` | Inventory rows that didn't survive the build | Reference only |
| `data/watcher-state-<view>.json` | Which tickets have been triaged for this view | Pruned to last 1000 entries automatically |
| `data/inbox-<view>.log` | Inbox runtime log (WARNING+ by default, DEBUG with `--verbose`) | Rotates? No — `tail -f` it; truncate when stale |

## Env vars (`.env`)

```
ZENDESK_SUBDOMAIN          # required — the part before .zendesk.com
ZENDESK_EMAIL              # required — the agent email used as Basic-auth user
ZENDESK_API_TOKEN          # required — Zendesk API token (NOT a password)

DD_API_KEY                 # optional — Datadog API key for triage/watch enrichment
DD_APP_KEY                 # optional — Datadog application key for triage/watch enrichment
DD_SITE                    # default datadoghq.com (eu = datadoghq.eu, us3, etc.)
DD_CALL_CENTER_TAG         # default @log.machineData.callCenterName
DD_STATION_TAG             # reserved for v2 station-level filtering; unused today

LLM_PROVIDER               # default unleash; set claude only for local fallback
UNLEASH_API_KEY            # required for production triage/watch LLM calls
UNLEASH_BASE_URL           # default https://e-api.unleash.so
UNLEASH_ASSISTANT_ID       # required dedicated triage assistant ID
UNLEASH_ACCOUNT            # optional; only for impersonated Unleash API keys

ANTHROPIC_MODEL            # Claude fallback model when LLM_PROVIDER=claude
```

Claude fallback reuses Claude CLI's OAuth session — there is intentionally **no `ANTHROPIC_API_KEY`** here.

## Common workflows

```bash
# "Investigate one ticket and prepare a local handoff draft"
triage-cli investigate 12345 --file ./station.log --save
# triage-notes/<id>-<ts>.md is paste-ready; no Zendesk write occurs

# "Fast one-shot triage with optional enrichment"
triage-cli triage 12345 --save

# "What's been happening overnight?"
triage-cli inbox --view 360123 --backfill 12h

# "Run the watcher on a server, browse the results from a laptop"
ssh server tmux new-session -d 'triage-cli watch --view 360123 --backfill 24h'
ssh -t server triage-cli inbox --view 360123     # attach for interactive review

# "Re-build the site map after inventory edits"
triage-cli build-map
git diff data/cnc-map.json data/cnc-map-gaps.md  # eyeball before committing

# "Quick triage with a specific time"
triage-cli triage 12345 --at 2026-05-07T14:03:00Z --window-minutes 30
```

## Troubleshooting

**`Error: ZENDESK_SUBDOMAIN/ZENDESK_EMAIL/ZENDESK_API_TOKEN must be set`** — `.env` not loaded or fields blank. Confirm `cat .env` shows values and you're running from the repo root.

**`could not resolve site for ticket; use --site or --cnc`** — `requester_org` doesn't match any `friendly_name` in the site map. Pass `--site <site_name>` or `--cnc <uuid>` explicitly, or add the customer to `apex-cnc-inventory.md` and re-run `build-map`.

**`LLM returned invalid TriageReport JSON after retry`** — the model produced malformed JSON twice. Re-run with `--verbose` to see the first-attempt failure logged. If persistent, the prompt may need a tweak in `triage_cli/llm.py:TRIAGE_SYSTEM_PROMPT`.

**`UNLEASH_API_KEY must be set` / `UNLEASH_ASSISTANT_ID must be set`** — production LLM mode needs Unleash credentials in `.env`.

**`Unleash API call failed with HTTP ...`** — check the Unleash API key scope, assistant ID, base URL, and optional impersonation account. Save the RequestId if one is printed.

**`inbox requires an interactive terminal`** — `inbox` was launched without a TTY (cron, pipe, redirected stdin). Use `watch` for headless runs.

**Inbox `y` says "No clipboard tool found"** — install one of `wl-copy` (Wayland), `xclip` (X11), or `pbcopy` (macOS).

**Inbox `o` shows the URL but no browser opens** — likely an SSH session with no `DISPLAY` or `BROWSER`. Copy the URL from the toast manually, or set `BROWSER=...` if you have a CLI browser.

**`No tickets matched. Last poll: …`** — view is empty, or the watcher hasn't seen any updates yet. Pass `--backfill 8h` (or longer) to seed from history.

**Two inboxes against the same view stomp each other's state** — known limitation, no file lock. Don't do that.

## Where to look in the code

| What | File |
| --- | --- |
| Triage system prompt | `triage_cli/llm.py:TRIAGE_SYSTEM_PROMPT` |
| Anchor extraction prompt | `triage_cli/llm.py:ANCHOR_EXTRACTION_PROMPT` |
| Pipeline orchestration | `triage_cli/pipeline.py:triage_one` |
| Site lookup logic | `triage_cli/extract.py:lookup_site` |
| Anchor priority | `triage_cli/extract.py:resolve_anchor` |
| Datadog query construction | `triage_cli/datadog.py:get_logs` |
| Watcher loop + state | `triage_cli/watcher.py` |
| Inbox app + actions | `triage_cli/inbox/app.py` |
| Renderer (Rich + markdown) | `triage_cli/render.py` |
| Schema | `triage_cli/models.py` |
| Detailed inbox runbook | `docs/runbooks/07-inbox-mode.md` |
| Design specs | `docs/superpowers/specs/` |
