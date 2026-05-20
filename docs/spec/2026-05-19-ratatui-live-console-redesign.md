# Ratatui live console redesign

Status: draft for implementation planning  
Date: 2026-05-19  
Scope: `triage-cli investigate`, `triage-cli inbox`, `triage-cli setup`, `triage-cli watch`, and `FORK_PACKET.md`

## Summary

The next product step is to make the terminal UI feel like a live investigation console instead of a static report viewer. The CLI already has the correct foundation: Rust, Ratatui, Crossterm, `tui-textarea`, `indicatif`, pipeline phase reporters, an inbox TUI, a chat pane, and five durable markdown artifacts.

The redesign should add motion and richer feedback where it clarifies operator state:

- What queue is being watched.
- Which ticket is active.
- Which phase is running.
- What changed since the last poll or revision.
- Which evidence moved the fork.
- What action the analyst owes next.

`INTAKE.md` is intentionally out of scope and should remain unchanged. `FORK_PACKET.md` may be refined because it is the committed routing artifact and the natural text counterpart to the animated decision-change UI.

## Principles

1. Keep stdout and stderr contracts stable.
2. Use animation to explain state changes, not as ambient decoration.
3. Prefer the existing Ratatui stack before adding dependencies.
4. Put TachyonFX behind a small animation facade so the app can run without effects.
5. Respect terminal constraints: reduced motion, non-TTY fallback, and small viewport behavior.
6. Keep markdown files as the audit trail; make the TUI the operator console.

## Current surfaces

### `triage-cli investigate`

Today this is the guided single-ticket path. It collects optional local evidence, runs the structured pipeline, writes the five markdown files, and prints `FORK_PACKET.md` to stdout. The default behavior must remain linear and pipeable.

Future TUI work should be opt-in. A single-ticket investigation console can return later after the inbox console proves the visual system.

### `triage-cli inbox`

This is the flagship surface for the redesign. It already has queue rows, background polling, on-demand triage, phase events from `Reporter`, a detail pane, file tabs, notifications, a site input modal, and a chat pane.

The current right pane is accurate but too document-like. It should become a case brief with live state, then offer artifact and chat tabs below it.

### `triage-cli setup`

Setup should remain calm and practical. It does not need a full-screen TUI. The right improvement is better TTY-only progress feedback for checks and writes, with plain output preserved for non-TTY runs.

### `triage-cli watch`

Watch is valuable as a headless polling mode, but it overlaps with inbox for human operators. The long-term shape should be:

- `inbox`: interactive monitored queue.
- `watch`: non-interactive/headless compatibility mode for scripts, daemons, and `--print-notes`.

Both should share the same polling engine.

## Implementation slices

### Slice 1: TUI animation foundation

Goal: introduce a small internal animation layer without changing command behavior.

Scope:

- Add an internal animation facade for Ratatui screens.
- Track named screen regions such as header, queue list, selected row, detail pane, notification, modal, phase band, and chat status.
- Add a reduced-motion gate.
- Add a runtime no-op path when animation is disabled or unsupported.
- Keep `inbox` visually identical unless effects are explicitly triggered by state transitions.

TachyonFX use:

- Evaluate TachyonFX as the first animation backend.
- Start with region-level effects rather than per-cell row effects.
- Do not rewrite the ticket table in this slice.

Acceptance:

- `cargo test` passes.
- `triage-cli inbox` renders without animation when reduced motion is enabled.
- Effects are never required for correctness.
- Non-TTY commands are unchanged.

Risk:

- Ratatui version compatibility. The project is currently on Ratatui 0.28, so dependency compatibility must be checked before landing TachyonFX.

### Slice 2: Inbox case brief redesign

Goal: replace the plain summary pane with a scan-first investigation brief.

Scope:

- Keep the existing queue list.
- Redesign the selected-ticket summary into an operational case brief:
  - Ticket ID.
  - Fork letter and label.
  - Confidence.
  - Status.
  - Owner.
  - Quoted rubric row.
  - Related Zendesk/Jira/master links.
  - Missing evidence.
  - Next action.
- Preserve existing file tabs.
- Keep `STATE.md` parsing lightweight; do not parse arbitrary markdown for the first pass.

Animation triggers:

- Selected ticket changes: detail pane reveal.
- Rubric mismatch: warning band pulse.
- Failed triage: brief red flash that settles into a readable error state.

Acceptance:

- Existing inbox tests for state parsing continue to pass.
- New tests cover case-brief rendering for high, medium, low, missing owner, related work, and rubric mismatch.
- Small terminals degrade to the existing simple summary or a compact brief.

Risk:

- The current table does not expose exact row rectangles. Keep row-level effects coarse until a custom list widget is justified.

### Slice 3: Interactive watch merged into inbox

Goal: make `inbox` the human-facing monitored queue while preserving `watch` for headless usage.

Scope:

- Extract shared polling/state logic from `watcher.rs` and `tui/inbox.rs`.
- Make `triage-cli inbox --view <id>` the preferred live queue command.
- Support assigned-item and view-based polling in inbox.
- Preserve `triage-cli watch --view <id>` as a non-interactive command.
- Keep watcher state file compatibility.

Command model:

```text
triage-cli inbox --view <id>
triage-cli inbox --assigned
triage-cli inbox --view <id> --interval 60
triage-cli watch --view <id>
```

Animation triggers:

- Poll started: subtle header sweep.
- Poll complete with no changes: quiet timestamp update.
- New ticket: queue list reveal.
- Ticket updated: row attention state.
- Ticket removed from queue: fade/settle transition if feasible.

Acceptance:

- `watch --print-notes` behavior remains intact.
- `inbox` can poll the same view as `watch`.
- Existing watcher state files still load.
- No polling code performs terminal rendering directly.

Risk:

- `watch` currently owns script-friendly output. Do not collapse the commands until shared internals are proven.

### Slice 4: Phase feedback and completion moments

Goal: make long-running investigation phases feel active and auditable.

Scope:

- Enrich phase labels in `PhaseReporter`.
- Add elapsed time for active phases where useful.
- Surface evidence counts during or after phase transitions.
- Improve completion and failure notifications.
- Keep all phase event names stable unless call sites are updated together.

Animation triggers:

- Phase advanced: progress band sweep.
- LLM call active: animated decision band or spinner with elapsed time.
- Save complete: completion toast plus row state settle.
- Soft warning accepted: amber warning band.

Acceptance:

- Pipeline correctness does not depend on animation.
- Stderr spinner behavior remains TTY-gated.
- Inbox phase progress still works when TachyonFX is disabled.

Risk:

- Too much movement during LLM calls can look noisy. Prefer slow, readable feedback.

### Slice 5: Chat pane polish

Goal: make follow-up investigation feel like part of the same console.

Scope:

- Improve in-flight provider status.
- Make attached evidence visible as compact labels.
- Distinguish analyst, Codex, automated, and system turns more clearly.
- Add a visible `/revise` transition state.
- Keep the JSONL conversation file as the source of truth.

Animation triggers:

- Message sent: input area settles/clears.
- Provider in flight: status-line throbber or TachyonFX band.
- Assistant response appended: transcript reveal.
- `/revise` complete: decision delta notification.

Acceptance:

- Existing chat parser and renderer tests pass.
- Transcript remains readable with animation disabled.
- Conversation writes remain lock-protected.

Risk:

- `throbber-widgets-tui` may require a Ratatui upgrade. Use existing frames first unless compatibility is confirmed.

### Slice 6: `FORK_PACKET.md` decision refinement

Goal: make the routing artifact match the console's decision model without changing `INTAKE.md`.

Scope:

- Keep `INTAKE.md` unchanged.
- Refine `FORK_PACKET.md` headings and ordering around the committed decision.
- Prefer a clearer top block:
  - Decision.
  - Decision signal.
  - Evidence used.
  - Missing evidence.
  - Related work.
  - Handoff.
- Consider an optional revision delta section only when a previous `STATE.md` exists.

Non-goals:

- Do not add invented before-content on first run.
- Do not parse prior markdown to infer history.
- Do not change stdout behavior: `triage` and `investigate` still print `FORK_PACKET.md`.

Acceptance:

- Existing ticket-folder tests are updated intentionally.
- Golden tests, once available, capture the new format.
- `FORK_PACKET.md` remains pasteable and reviewable.

Risk:

- This is a user-visible artifact. Keep the change small and explain it in the changelog or PR body.

### Slice 7: Setup feedback pass

Goal: improve first-run confidence without making setup theatrical.

Scope:

- Keep setup as a normal terminal command.
- Improve check rows for env vars, path checks, map generation, scratch writability, and provider readiness.
- Use TTY-only spinner/status feedback where probes take time.
- Keep non-TTY output plain.

Animation triggers:

- Probe in progress: lightweight spinner.
- Probe pass/fail: clear status marker.
- Setup complete: concise completion line.

Acceptance:

- Setup remains idempotent.
- Doctor remains script-friendly.
- No alternate-screen UI is introduced.

Risk:

- Setup is not the flagship surface. Keep this slice short.

## Recommended order

1. Slice 1: TUI animation foundation.
2. Slice 2: Inbox case brief redesign.
3. Slice 4: Phase feedback and completion moments.
4. Slice 3: Interactive watch merged into inbox.
5. Slice 5: Chat pane polish.
6. Slice 6: `FORK_PACKET.md` decision refinement.
7. Slice 7: Setup feedback pass.

Reasoning: prove animation architecture in the safest screen first, then improve the inbox experience, then merge watch behavior after the visual and state model are stable.

## Open questions

- Does TachyonFX support the current Ratatui version, or do we need a Ratatui upgrade first?
- Should animation be controlled by an env var, a config key, or both?
- Should `watch` eventually print a TTY hint pointing operators to `inbox --view`?
- What is the minimum small-terminal layout for the redesigned case brief?
- Should revision delta live only in the TUI, or also in `FORK_PACKET.md` when prior state exists?
