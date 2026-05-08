# triage-watch

Watcher skeleton for a Zendesk view. The spike persists state file
shape and logs intent only — it does not poll Zendesk yet.

## When to use

- You want to validate the watcher state file shape.
- You're wiring `triage-cli watch` into a scheduler / launchd /
  cron and need a no-op tick for plumbing tests.
- You're picking up the watcher implementation from this spike and
  need a known starting point.

## Basic usage

```bash
triage-cli watch --view 12345
```

What happens:
- `→ watcher tick: view=12345 (would poll Zendesk and triage updated tickets — not implemented in spike)` on stderr.
- State loaded (or initialized) from
  `./triage-notes/.watcher/watcher-state-<view-id>.json`.
- State written back atomically (tempfile + rename), preserving
  schema version 1.

## Flags

| Flag | Purpose |
| --- | --- |
| `--view <id>` | Required. Zendesk view ID. |
| `--interval <duration>` | Poll interval (unused in spike, default `60s`). |
| `--once` / `--continuous` | Default is `--once`. Continuous mode logs a stub message and exits. |
| `--output-dir <dir>` | State lives under `<output-dir>/.watcher/`. |

## State file shape

```json
{
  "version": 1,
  "triaged": {
    "12345": "2026-05-08T14:30:00Z"
  }
}
```

`triaged` maps ticket ID to the most recently observed `updated_at`.
A real watcher will re-triage when that timestamp advances and
silently backfill on first run.

## What this is not (yet)

- No Zendesk view query.
- No re-triage logic.
- No prune-to-1000-entries policy.
- No first-run silent backfill.

These are the open work items for the next agent. The state file
shape and atomic write path are in place so they can be implemented
without disturbing on-disk consumers.
