# triage-cli cheatsheet

## What it is

A Rust CLI that investigates and triages Zendesk tickets for the Carbyne APEX
NG911/E911 platform. A successful run writes the five-file ticket folder
`${TRIAGE_TICKETS_ROOT:-./Tickets}/<id>/` and prints `FORK_PACKET.md` to stdout.

| Command | Purpose |
| --- | --- |
| `investigate <id>` | Guided investigation: fetch ticket, collect evidence, run the structured pipeline. Requires TTY. |
| `triage <id>` | Headless single-shot using the same pipeline, with no evidence prompts. |
| `doctor` | Check env vars, credentials, ticket-root and scratch-dir writability. Exits 0/1. |
| `inbox --view N` | Interactive ratatui ticket-folder viewer. Use this at the keyboard. |
| `watch --view N` | Headless poll loop over a Zendesk view. |
| `build-map` | Regenerate `data/cnc-map.json` from `apex-cnc-inventory.md`. |
| `setup` | Interactive first-run setup; writes `.env`. |

## First-time setup

```bash
triage-cli setup
triage-cli build-map
triage-cli doctor
```

## `doctor` - environment check

```bash
triage-cli doctor
```

Exits 0 when all critical checks pass. Datadog is a warning only. The scratch
workspace lives under `$TRIAGE_HOME/scratch/`; ticket folders are written under
`${TRIAGE_TICKETS_ROOT:-./Tickets}/`.

## `investigate <id>` - guided investigation

```bash
triage-cli investigate 12345
triage-cli investigate 'https://acme.zendesk.com/agent/tickets/12345'
triage-cli investigate 12345 --file ./station.log
triage-cli investigate 12345 --paste 'console=WARN audio dropped'
triage-cli investigate 12345 --file ./station.log --paste 'dispatch=PTT failures'
triage-cli investigate 12345 --no-llm        # skip LLM; deterministic report
triage-cli investigate 12345 --tui           # progress TUI (requires TTY)
triage-cli investigate 12345 --verbose
```

`investigate` always writes `Tickets/<id>/{INTAKE,EVIDENCE_PREFLIGHT,FORK_PACKET,DRAFTS,STATE}.md`
and prints `FORK_PACKET.md` to stdout. It never posts to Zendesk, Jira, or any
audited external surface.

## `triage <id>` - one-shot

```bash
triage-cli triage 12345
triage-cli triage 'https://acme.zendesk.com/agent/tickets/12345'
triage-cli triage 12345 --verbose
triage-cli triage 12345 --no-logs
triage-cli triage 12345 --no-llm
triage-cli triage 12345 --window-minutes 60
triage-cli triage 12345 --at 2026-05-07T14:03:00Z
triage-cli triage 12345 --site us-co-aurora-apex
triage-cli triage 12345 --cnc 921d7c53-e815-...
triage-cli triage 12345 --levels error,warn,info
triage-cli triage 12345 --force
triage-cli triage 12345 --diff
```

`triage` is the headless form of the same structured pipeline. It writes the
ticket folder and prints `FORK_PACKET.md`, so it is still pipeable for handoff.

## Offline fixture smoke

```bash
triage-cli triage 55001 --fixture triage-cli-rs/fixtures/audio-drop --no-llm --force
triage-cli demo audio-drop
```

These fixture paths load canned ticket, Datadog, and memory inputs. They do
not require Zendesk creds, Datadog creds, or a prebuilt
`$TRIAGE_HOME/data/cnc-map.json`.

## `inbox --view N` - interactive TUI

```bash
triage-cli inbox --view 360123
triage-cli inbox --view 360123 --poll 30
triage-cli inbox --view 360123 --backfill 8h
triage-cli inbox --view 360123 --backfill inf
triage-cli inbox --view 360123 --window-minutes 30 --levels error,warn,info
triage-cli inbox --view 360123 --verbose
```

Inbox scans `${TRIAGE_TICKETS_ROOT:-./Tickets}/` for ticket folders containing
`STATE.md`. It shares `data/watcher-state-<view>.json` with `watch`.

### Keys

| Key | Action |
| --- | --- |
| Up/Down or `k`/`j` | Move cursor |
| `enter` | Triage queued row, or focus detail pane |
| `tab` / `shift-tab` | Cycle file tabs |
| `escape` | Return to synth view / list |
| `r` | Force an immediate poll |
| `y` | Copy the selected synth summary |
| `o` | Open the ticket in Zendesk |
| `q` / `Ctrl-C` | Quit |

## `watch --view N` - headless

```bash
triage-cli watch --view 360123
triage-cli watch --view 360123 --interval 30
triage-cli watch --view 360123 --backfill 24h
triage-cli watch --view 360123 --print-notes
triage-cli watch --view 360123 --no-logs
triage-cli watch --view 360123 --state-file /tmp/state.json
```

Use `tmux`, `nohup`, or `systemd` to keep it running. The watcher writes
ticket folders for new or updated tickets and stores progress in
`data/watcher-state-<view>.json`.

## `build-map`

```bash
triage-cli build-map
```

Run after editing `apex-cnc-inventory.md`. `triage`, `investigate`, and
watcher site resolution depend on the JSON map.

## Where things live

| Path | What | Cleanup |
| --- | --- | --- |
| `Tickets/<id>/INTAKE.md` | Engine-known facts and initial route | Keep with ticket folder |
| `Tickets/<id>/EVIDENCE_PREFLIGHT.md` | Gathered, decisive, and missing evidence | Keep with ticket folder |
| `Tickets/<id>/FORK_PACKET.md` | Pipeable handoff and committed fork | Keep with ticket folder |
| `Tickets/<id>/DRAFTS.md` | CONFIRM-gated customer/internal/Jira drafts | Keep with ticket folder |
| `Tickets/<id>/STATE.md` | Machine-readable fork/status for inbox and soft-locks | Keep with ticket folder |
| `$TRIAGE_HOME/scratch/<id>/` | Transient interactive workspace for attachments/local evidence | Manual cleanup |
| `data/cnc-map.json` | Site map generated by `build-map` | Do not edit by hand |
| `data/cnc-map-gaps.md` | Inventory rows skipped during conversion | Reference only |
| `data/watcher-state-<view>.json` | Which tickets have been triaged for this view | Pruned to 1000 entries |
| `data/inbox-<view>.log` | Inbox runtime log | Truncate when stale |

## Env vars

| Variable | Purpose |
| --- | --- |
| `TRIAGE_HOME` | Per-user data root for `.env`, memory, inventory, `data/`, and `scratch/`. |
| `TRIAGE_TICKETS_ROOT` | Ticket-folder root. Default: `./Tickets`. |
| `TRIAGE_OWNER` | Owner recorded in `STATE.md`; used by soft-lock checks. |
| `LLM_PROVIDER` | `unleash` by default; `codex` also supported. |
| `UNLEASH_API_KEY`, `UNLEASH_ASSISTANT_ID` | Required when `LLM_PROVIDER=unleash`. |
| `CODEX_MODEL` | Model for the `codex` provider. Default: `gpt-5.5`. |
| `ZENDESK_SUBDOMAIN`, `ZENDESK_EMAIL`, `ZENDESK_API_TOKEN` | Required Zendesk credentials. |
| `DD_API_KEY`, `DD_APP_KEY` | Optional Datadog enrichment credentials. |
| `DD_SITE`, `DD_CALL_CENTER_TAG`, `DD_STATION_TAG` | Datadog query settings; station tag is reserved. |

## Common workflows

```bash
# Investigate one ticket and prepare the local ticket folder.
triage-cli investigate 12345 --file ./station.log

# Fast one-shot triage with optional enrichment.
triage-cli triage 12345

# Browse overnight ticket folders while polling the view.
triage-cli inbox --view 360123 --backfill 12h

# Run the watcher on a server, browse results from a laptop.
ssh server tmux new-session -d 'triage-cli watch --view 360123 --backfill 24h'
ssh -t server triage-cli inbox --view 360123

# Rebuild the site map after inventory edits.
triage-cli build-map
git diff data/cnc-map.json data/cnc-map-gaps.md
```

## Troubleshooting

**`Error: ZENDESK_SUBDOMAIN/ZENDESK_EMAIL/ZENDESK_API_TOKEN must be set`** -
`.env` is not loaded or fields are blank.

**`could not resolve site for ticket; use --site or --cnc`** - pass
`--site <site_name>` or `--cnc <uuid>`, or update `apex-cnc-inventory.md` and
rerun `build-map`.

**`STATE.md soft-lock conflict: owned by <other-analyst>`** - another analyst
already wrote this ticket folder. Re-run with `--diff` to inspect the change or
`--force` to overwrite after coordinating.

**`inbox requires an interactive terminal`** - use `watch` for headless runs.

**Inbox shows an old `rubric_version` banner** - the selected `STATE.md` was
written under a different rubric version. The artifact is honored as-is.

## Where to look in the code

| What | File |
| --- | --- |
| CLI surface | `triage-cli-rs/src/cli.rs` |
| Structured pipeline | `triage-cli-rs/src/pipeline.rs` |
| Ticket-folder writer | `triage-cli-rs/src/ticket_folder.rs` |
| Inbox app | `triage-cli-rs/src/tui/inbox.rs` |
| Watcher loop/state | `triage-cli-rs/src/watcher.rs` |
| Site lookup logic | `triage-cli-rs/src/extract.rs` |
| Datadog query construction | `triage-cli-rs/src/datadog.rs` |
| LLM prompts/providers | `triage-cli-rs/src/llm.rs`, `triage-cli-rs/src/providers/` |
| Detailed inbox runbook | `docs/runbooks/07-inbox-mode.md` |
