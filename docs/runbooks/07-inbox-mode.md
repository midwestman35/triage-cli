# Runbook 07 - Inbox mode

> **When to use this:** you want an interactive terminal view over produced
> ticket folders while polling a Zendesk view. For unattended or headless runs,
> use `triage-cli watch`.

`triage-cli inbox --view <id>` launches a ratatui TUI. The left pane is the
ticket list; the right pane defaults to a synthetic summary parsed from the
selected ticket's `STATE.md`. `Tab` switches into the per-file view across the
five ticket-folder markdown files.

## Prerequisites

- The standard `triage-cli` env (`ZENDESK_*`, optional `DD_*`, and credentials
  for the selected `LLM_PROVIDER`; see `docs/runbooks/05-switching-models.md`).
- The numeric Zendesk view ID. Find it in the Zendesk UI: open the view and the
  URL ends in `/views/<id>`.
- A built site map at `data/cnc-map.json` (run `triage-cli build-map` if
  missing).
- An interactive terminal. Non-TTY launches fail fast; use `watch` for cron,
  systemd, CI, or shell pipelines.

## Launch

```bash
triage-cli inbox --view 360123 --poll 60
```

Flags mirror the overlapping `watch` fields:

- `--view` Zendesk view ID to monitor.
- `--poll` seconds between polls. Default: `60`; minimum: `10`.
- `--backfill` initial backfill horizon: `0`, `Nh`, `Nd`, or `inf`. Default:
  `0`.
- `--window-minutes` Datadog search radius around the ticket anchor. Default:
  `15`.
- `--levels` comma-separated Datadog log levels. Default: `error,warn`.
- `--no-logs` skips Datadog enrichment.
- `--verbose` enables verbose runtime behavior where supported.

On startup the inbox scans `${TRIAGE_TICKETS_ROOT:-./Tickets}/` for
subdirectories containing `STATE.md`. Each matching `Tickets/<id>/STATE.md`
becomes one inbox row. The app then starts an immediate poll and continues
polling on the `--poll` interval.

## Navigation

| Key | Action |
| --- | --- |
| Up/Down or `k`/`j` | navigate the ticket list |
| `enter` | triage a queued row, or focus the detail pane otherwise |
| `tab` / `shift-tab` | cycle into and through the file tabs |
| `escape` | return to synth view / list |
| `r` | force a refresh / immediate poll |
| `y` | copy the selected ticket's synth summary |
| `o` | open the selected ticket in Zendesk |
| `q` or Ctrl-C | quit |

The default detail view is parsed from `STATE.md`: fork letter, confidence,
quoted rubric row, status, owner, and related tickets. The file-tab view reads
`INTAKE.md`, `EVIDENCE_PREFLIGHT.md`, `FORK_PACKET.md`, `DRAFTS.md`, and
`STATE.md` from the selected ticket folder. If no row is selected, the detail
pane prompts you to select a ticket.

If a clipboard tool is missing, install one of `wl-copy`, `xclip`, or `pbcopy`.
Opening Zendesk requires `ZENDESK_SUBDOMAIN` in the environment; the app also
shows the URL in a toast.

## Status Icons

The ticket list uses the current inbox status icons:

- `✓` triaged
- `→` triaging
- `○` pending
- `✗` failed

Rows sort by status group first: triaging, triaged, pending, then failed.
Triaged rows sort newest-first by `generated_at`.

## Persistence

- `${TRIAGE_TICKETS_ROOT:-./Tickets}/<id>/STATE.md` - source of inbox rows and
  the default synth summary.
- `${TRIAGE_TICKETS_ROOT:-./Tickets}/<id>/{INTAKE,EVIDENCE_PREFLIGHT,FORK_PACKET,DRAFTS,STATE}.md`
  - per-file tab content.
- `data/watcher-state-<view>.json` - shared watch/inbox state for the same view.
  Tickets already triaged at a matching `updated_at` are shown as unchanged
  rather than reprocessed.
- `data/inbox-<view>.log` - runtime warnings and watcher status emitted while
  the TUI is running.

Legacy `triage-notes/*.json` sidecars are not part of the v1 inbox contract.
Inbox hydration comes from ticket folders only.

## Troubleshooting

**"inbox requires an interactive terminal."** You launched the TUI in a pipeline
or non-TTY context. Use `triage-cli watch --view <id>` for headless runs.

**Empty list on launch.** No ticket folders with `STATE.md` were found under
`${TRIAGE_TICKETS_ROOT:-./Tickets}/`. The first poll should add pending rows for
tickets currently in the view. If it does not, check `data/inbox-<view>.log`.

**A ticket folder exists but does not show up.** Hydration reads
`Tickets/<id>/STATE.md`. Check that the folder name is the numeric Zendesk
ticket ID and that `STATE.md` is present and readable.

**"No clipboard tool found."** Install `wl-copy` on Wayland, `xclip` on X11, or
`pbcopy` on macOS.

**"ZENDESK_SUBDOMAIN not set."** Set the Zendesk subdomain in the environment
before using `o`.

**Two inboxes against the same view.** Avoid this: there is no inbox-level file
lock, and the same `data/watcher-state-<view>.json` path is shared.
