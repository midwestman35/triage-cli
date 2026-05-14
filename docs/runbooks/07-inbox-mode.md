# Runbook 07 - Inbox mode

> **When to use this:** you want an interactive terminal view of saved triage
> reports for tickets in a Zendesk view, with the ticket list and selected
> report visible side by side. For unattended or headless runs, use
> `triage-cli watch`.

`triage-cli inbox --view <id>` launches a Textual TUI. The left pane is the
ticket list; the right pane renders the selected `TriageReport`.

## Prerequisites

- The standard `triage-cli` env (`ZENDESK_*`, `DD_*`, and the credentials for the selected `LLM_PROVIDER` — see `docs/runbooks/05-switching-models.md`).
- The numeric Zendesk view ID. Find it in the Zendesk UI: open the view and
  the URL ends in `/views/<id>`.
- A built site map at `data/cnc-map.json` (run `triage-cli build-map` if
  missing).
- An interactive terminal. Non-TTY launches fail fast; use `watch` for cron,
  systemd, CI, or shell pipelines.

## Launch

```bash
triage-cli inbox --view 360123 --poll 60
```

Flags mirror the overlapping `watch` fields:

- `--view` Zendesk view ID to monitor. Required.
- `--poll` seconds between polls. Default: `60`; minimum: `10`.
- `--backfill` initial backfill horizon: `0`, `Nh`, `Nd`, or `inf`. Default:
  `0`.
- `--window-minutes` Datadog search radius around the ticket anchor. Default:
  `15`.
- `--levels` comma-separated Datadog log levels. Default: `error,warn`.
- `--verbose` enables verbose runtime behavior where supported.

The inbox hydrates recent `triage-notes/*.json` sidecars on startup, starts an
immediate poll, then continues polling on the `--poll` interval. It shares the
same watcher state file as `watch`, so tickets already triaged at a matching
`updated_at` are shown as unchanged rather than reprocessed.

## Navigation

| Key | Action |
| --- | --- |
| Up/Down or `k`/`j` | navigate the ticket list |
| `enter` | move focus to the detail pane |
| `escape` | return focus to the ticket list |
| `r` | force a refresh / immediate poll |
| `y` | copy the selected report's suggested note |
| `o` | open the selected ticket in Zendesk |
| `q` or Ctrl-C | quit |

When the cursor moves in the list, the detail pane updates to the selected
report. If no row is selected, the detail pane shows
`Select a ticket to view its report.`

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

- `triage-notes/<id>-<ts>.md` - paste-ready markdown, written when a triage is
  saved.
- `triage-notes/<id>-<ts>.json` - structured `TriageReport`, used by inbox
  hydration on startup.
- `data/watcher-state-<view>.json` - shared watcher state for the same view.
- `data/inbox-<view>.log` - runtime warnings and watcher status emitted while
  the TUI is running.

## Troubleshooting

**"inbox requires an interactive terminal."** You launched the TUI in a
pipeline or non-TTY context. Use `triage-cli watch --view <id>` for headless
runs.

**Empty list on launch.** No recent JSON sidecars were found in
`triage-notes/`. The first poll should add pending rows for tickets currently
in the view. If it does not, check `data/inbox-<view>.log`.

**A report exists but does not show up.** Hydration reads recent structured
sidecars, not markdown-only notes. Check that a matching
`triage-notes/<id>-<ts>.json` file exists and is valid.

**"No clipboard tool found."** Install `wl-copy` on Wayland, `xclip` on X11, or
`pbcopy` on macOS.

**"ZENDESK_SUBDOMAIN not set."** Set the Zendesk subdomain in the environment
before using `o`.

**Two inboxes against the same view.** Avoid this: there is no inbox-level file
lock, and the same `data/watcher-state-<view>.json` path is shared.
