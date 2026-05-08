# Investigation-progress TUI (deferred spec)

**Status:** spec only; not scheduled. Build when the linear flow becomes a
complaint.
**Date:** 2026-05-08
**Origin:** ported from the Go spike's `--tui` flow on `archive/go-spike`.

## Problem

`triage-cli triage` and `investigate` emit progress to stderr as
`→ [N/6] <phase status>` lines. The LLM call inside `pipeline.triage_one`
typically takes 15–40 seconds. During that window the linear flow looks
identical to a hang — no spinner movement, no partial output, no indication
of which phase is running. New operators routinely Ctrl-C in the assessment
phase thinking the tool is stuck.

The orbit spinner (`unicode-animations`) helps for sub-second waits but is
the wrong shape for multi-second pipeline phases that have meaningful
sub-state (which evidence is being reviewed, how many comments, etc.).

## Goal

An opt-in TUI for a single investigation that makes the pipeline state
legible while it runs, and turns into a report viewer when it completes.
One ticket, one pipeline, one screen — **not** an inbox or a queue. The
Textual inbox at `triage_cli/inbox/` already covers the multi-ticket case.

## Non-goals

- Multi-ticket views. Use the inbox.
- Editing or annotating the investigation. View only.
- Running multiple pipelines in parallel.
- Replacing the linear stderr flow. Default stays linear so output remains
  pipeable; the TUI is behind `--tui`.
- Live tailing of LLM token streams. Phase-level granularity is enough.

## Layout

Three regions, rendered at ≥80×24:

```
triage-cli · ZD-12345 · SBC jitter on PSAP-01 · running
sources: zendesk, local-file
╭──────────────────────────╮╭──────────────────────────────────────────────╮
│Workflow                  ││Review comments                               │
│                          ││                                              │
│✓ Load ticket             ││Reviewing comments (3 found)...               │
│→ Review comments         ││                                              │
│• Catalogue attachments   ││Ticket: SBC jitter on PSAP-01                 │
│• Ingest evidence         ││Requester: Acme Co                            │
│• Build timeline          ││                                              │
│• Assess                  ││                                              │
╰──────────────────────────╯╰──────────────────────────────────────────────╯
╭──────────────────────────────────────────────────────────────────────────╮
│Evidence / Timeline                                                       │
│                                                                          │
│[2/6] Reviewing comments (3 found)...                                     │
│loaded ZD-12345: SBC jitter on PSAP-01                                    │
╰──────────────────────────────────────────────────────────────────────────╯
[q] quit · running pipeline…
```

- **Header.** Ticket id, subject, top-level state (`running` / `complete` /
  `error` / `cancelled`). Sources line below lists the evidence sources
  contributing to this run.
- **Workflow rail (left, ~⅓ width).** Per-phase status line with
  `✓` / `→` / `•` / `✗` glyphs. Static order; no scrolling.
- **Active-step pane (right, ~⅔ width).** Detail for the currently
  running (or last completed, post-run) phase: status sub-line plus the
  ticket summary or evidence list relevant to that step.
- **Evidence/Timeline pane (full width, bottom).** Append-only log of
  phase transitions and the timeline as it builds. Scrollable.
- **Footer.** Active key hints; switches to a report-viewer hint set when
  the pipeline completes.

When the pipeline finishes, the right pane swaps to a Markdown viewer of
the rendered `TriageReport`; the workflow rail freezes with all `✓`s.

## Phases

Mirror `pipeline.triage_one` / the investigation flow:

1. Load ticket
2. Review comments
3. Catalogue attachments
4. Ingest evidence (local files / pasted text, when present)
5. Build timeline
6. Assess (LLM call)

Status states: `pending` (•), `active` (→), `done` (✓), `failed` (✗).

## Key bindings

| Key | Action |
| --- | --- |
| `q`, `ctrl+c` | Quit; cancels the pipeline if it's still running |
| `tab` / `shift+tab` | Cycle focus between Active, Timeline, and (post-completion) Report |
| `↑` / `↓` / `pgup` / `pgdn` | Scroll the focused viewport |
| `enter` | After completion, focus the report viewer |

## Implementation notes (Python / Textual)

- Library: Textual (already a runtime dep for the inbox). No new dep.
- Mutually exclusive with `--json` and `--quiet`. Error early with a typed
  message if combined; the TUI takes over the screen and can't coexist
  with stdout-stream output.
- Refuse to run without a TTY. Print a hint and a fallback command:
  > `--tui requires a TTY; rerun without --tui to use the linear flow.`
- Pipeline integration: have `pipeline.triage_one` (and the investigation
  service) accept a `Reporter` protocol with `phase_started(phase)`,
  `phase_done(phase, detail)`, `phase_failed(phase, err)`, `evidence_added(item)`,
  `done(report)`. Default reporter is the existing stderr logger; the
  TUI provides a reporter that pushes events onto a queue the Textual app
  consumes. The Go branch landed on the same shape — see
  `internal/investigation/flow.go` at `archive/go-spike`.
- Cancellation: Ctrl-C / `q` cancels the asyncio task running
  `pipeline.triage_one` and prints `→ cancelled` to stderr after the TUI
  exits cleanly. No artifact is written on cancellation.
- Save behavior: identical to the linear flow. Paired `.md` and `.json`
  written to `./triage-notes/<id>-<ts>` when the pipeline completes
  successfully.

## When to build

Trigger conditions, any of:
- Two or more operators report the linear flow looks hung during the
  assessment phase.
- We move to a slower model (`claude-opus-*`) where assessment routinely
  exceeds 30 seconds.
- We add evidence sources that themselves take measurable time
  (attachment download + extraction, Datadog log windows over a long
  range), making the existing single-line stderr too thin.

Until one of those, the linear flow plus a more informative spinner caption
on the assessment phase is enough.

## Reference

- Go implementation (archived): `triage-cli-go/internal/tui/` at tag
  `archive/go-spike`. Look at `model.go`, `view.go`, `update.go` for the
  state machine; `program.go` for the cancellation/reporter wiring.
- Lessons: `docs/lessons-from-go-spike.md`.
