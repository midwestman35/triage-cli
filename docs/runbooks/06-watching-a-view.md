# Watch a Zendesk view

> **When to use this:** you want continuous triage notes for tickets in a specific Zendesk view — say, "Open Tier-1 incidents" — without running `triage-cli triage` by hand for every new ticket or comment update.

## Prerequisites

- The standard `triage-cli` env (`ZENDESK_*`, `DATADOG_*`, `ANTHROPIC_MODEL`).
- The numeric Zendesk view ID. Find it in the Zendesk UI: open the view and
  the URL ends in `/views/<id>`.
- A built site map at `data/cnc-map.json` (run `triage-cli build-map` if
  missing).

## First run

```bash
triage-cli watch --view 12345
```

Default behavior:

- **Backfill horizon:** 24 hours. Every ticket whose `updated_at` is within
  the last 24 hours gets triaged on first run; older tickets are recorded as
  "already seen" without producing notes.
- **Interval:** sleep 300 seconds between iterations.
- **Output:** notes saved to `./triage-notes/`. stderr carries one status
  line per ticket. stdout is silent.

Want every ticket in the view triaged on first run? `--backfill inf`.
Want no notes at all on first run, only future updates? `--backfill 0`.

## Reading the stderr stream

```
[14:32:01] iteration 1 start (view=12345)
[14:32:04] #98765 triaged → triage-notes/98765-20260507T143204Z.md
[14:32:31] #98766 skipped: site unresolvable
[14:33:08] #98767 failed: Datadog timeout (will retry)
[14:33:09] #98768 unchanged
[14:33:09] iteration 1 done; sleeping 300s
```

Status verbs:
- `triaged` — note generated and saved.
- `unchanged` — ticket's `updated_at` matches what we triaged before.
- `skipped: site unresolvable` — ticket subject/description/org didn't match
  the site map. State is **not** marked, so the next time this ticket gets
  touched (or you fix the site map) it'll retry.
- `failed: <reason> (will retry)` — transient error (Datadog timeout, Claude
  rate limit, Zendesk 5xx). State is **not** marked; the next iteration
  retries.

## State file

Path: `data/watcher-state-<view-id>.json` (default) or whatever you passed to
`--state-file`. Schema:

```json
{
  "version": 1,
  "triaged": {
    "98765": "2026-05-07T14:32:04+00:00"
  }
}
```

The watcher prunes the file to the 1000 most-recent entries on every save,
so it doesn't grow unbounded.

## Recovering from accidental state deletion

If you delete the state file, the watcher treats every ticket within the
backfill horizon as "new" and re-triages them. Cost: `2 × N` Claude calls
where `N` is the number of tickets in the horizon. To avoid the burst, run
once with `--backfill 0` after restoring; that re-marks every ticket in the
view as "seen" without producing notes, then your next watcher invocation
catches only future updates.

## Stopping the watcher

`Ctrl-C` exits cleanly: the current iteration finishes its in-flight ticket
(no abort mid-LLM-call), state is saved, and the process exits 0.

## Two views at once

Run two watchers in two terminals; their default state files are different
(`data/watcher-state-123.json` vs. `data/watcher-state-456.json`), so they
do not collide.
