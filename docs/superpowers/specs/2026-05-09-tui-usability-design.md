# TUI Usability Improvements — Design Spec

**Date:** 2026-05-09
**Branch:** pipelinev2
**Scope:** Four additive changes to the inbox TUI that improve scan speed, feedback during
triage, and failure visibility. No new dependencies. No behavior changes outside `inbox/` and
`pipeline.py`.

---

## Overview

Four features, in implementation order:

| # | Feature | Files changed |
|---|---------|---------------|
| 1 | Relative timestamps | `inbox/widgets.py` |
| 2 | Color-coded confidence badges | `inbox/widgets.py` |
| 3 | Progress bar + phase labels | `inbox/widgets.py`, `inbox/app.py`, `pipeline.py` |
| 4 | Per-row status coloring | `inbox/widgets.py` |

Features 1, 2, and 4 are self-contained to `widgets.py`. Feature 3 is the only cross-file change.

---

## Feature 1 — Relative timestamps

### Goal

Replace the absolute `%H:%M` clock time in the ticket list "When" column with a human-relative
string that communicates ticket age at a glance without mental arithmetic.

### Implementation

Add a pure function to `inbox/widgets.py`:

```python
def _relative_time(dt: datetime) -> str:
    delta = datetime.now(UTC) - dt
    minutes = int(delta.total_seconds() / 60)
    if minutes < 2:
        return "just now"
    if minutes < 60:
        return f"{minutes}m ago"
    hours = minutes // 60
    if hours < 24:
        return f"{hours}h ago"
    return f"{delta.days}d ago"
```

In `refresh_rows`, replace:

```python
when = report.generated_at.strftime("%H:%M") if report is not None else "—"
```

with:

```python
when = _relative_time(report.generated_at) if report is not None else "—"
```

### Rules

| Age | Display |
|-----|---------|
| < 2 min | `just now` |
| < 60 min | `14m ago` |
| < 24 h | `3h ago` |
| ≥ 24 h | `2d ago` |

---

## Feature 2 — Color-coded confidence badges

### Goal

Make the "Conf" column scannable by color rather than requiring the user to read the string.

### Implementation

Add a lookup dict to `inbox/widgets.py`:

```python
_CONFIDENCE_STYLE: dict[str, str] = {
    "high":   "[bold green]high[/]",
    "medium": "[yellow]med[/]",
    "low":    "[red]low[/]",
}
```

In `refresh_rows`, replace:

```python
confidence = report.confidence if report is not None else "—"
```

with:

```python
confidence = _CONFIDENCE_STYLE.get(report.confidence, report.confidence) if report is not None else "—"
```

---

## Feature 3 — Progress bar + phase labels

### Goal

Eliminate the 15–40 second "frozen" appearance when a ticket is triaging. Show the current
pipeline phase by name and a visual progress bar that advances through 4 steps.

### Architecture

Three coordinated changes:

1. `pipeline.triage_one` emits phase events via an optional callback.
2. `inbox/app.py` wires the callback for user-initiated triage; watcher-driven triage is unaffected.
3. `ReportPaneWidget` is restructured from `Static` to a compound `Widget` with three display modes.

### 3a — `pipeline.triage_one` callback

Add one optional parameter:

```python
on_phase: Callable[[str, int], None] | None = None
```

Called as `on_phase(label, step)` at three points inside `triage_one`:

| Step | Label | Location |
|------|-------|----------|
| 2 | `"Extracting anchor timestamp"` | before anchor LLM call |
| 3 | `"Querying Datadog"` | before `dd_client.get_logs` |
| 4 | `"Asking Claude"` | before `_llm_triage` |

Total steps is always 4. Step 1 (`"Fetching ticket"`) fires from `app.py` before calling
`triage_one` (see 3b), so the bar starts at 25% immediately when the row flips to triaging.
Steps 2-4 advance the bar to 50%, 75%, 100% — the bar reaches 100% at the last phase, then
the report replaces the progress region on completion.
If `on_phase` is `None`, no calls are made — all existing callers (`watcher.run_iteration`,
`cli.triage`) are unaffected.

### 3b — `app.py` wiring

`_triage_ticket_blocking` fires step 1 manually before calling `_run_pipeline_blocking`:

```python
self.call_from_thread(self._set_phase, ticket_id, "Fetching ticket", 1)
```

`_run_pipeline_blocking` builds a closure and passes it:

```python
def _phase(label: str, step: int) -> None:
    self.call_from_thread(self._set_phase, ticket_id, label, step)

report = pipeline.triage_one(..., on_phase=_phase)
```

`_set_phase(ticket_id, label, step)` updates `RowEntry.phase_label` and `RowEntry.phase_step`,
then calls `show_progress` on the detail pane if that ticket is currently selected.

**Watcher path:** `_run_iteration_blocking` calls `watcher.run_iteration` which calls
`triage_one` internally. It passes `on_phase=None`. The `_on_progress` callback continues to
flip rows to `"triaging"` as before. No detail pane progress is shown for watcher-driven triage —
this is acceptable because watcher triage is background polling, not user-initiated.

### 3c — `ReportPaneWidget` restructure

`ReportPaneWidget` changes from `Static` to `Widget`. Children composed at mount:

```
ReportPaneWidget (Widget)
├── Static          id="report-body"
└── Vertical        id="progress-region"
    ├── Label       id="phase-label"
    └── ProgressBar id="phase-bar"       total=4
```

Three public methods:

**`show_report(report: TriageReport)`**
Hides `#progress-region`, shows `#report-body` with `rich_layout(report)`.

**`show_progress(label: str, step: int, total: int = 4)`**
Hides `#report-body`, shows `#progress-region`. Updates `#phase-label` text to label.
Advances `#phase-bar` to `step`.

**`show_placeholder(text: str)`**
Hides `#progress-region`, shows `#report-body` with plain markup text (no report object).
Reuses the same `Static` child as `show_report` — no third child needed. Keeps existing
queued/failed placeholder behavior unchanged.

#### `RowEntry` additions

```python
phase_label: str | None = None
phase_step: int = 0
```

Stored so that navigating to a ticket that is mid-triage shows the correct step rather than
resetting the bar to zero.

#### Detail pane update path

`on_data_table_row_highlighted` checks the selected entry's status:
- `"triaged"` → `show_report(entry.report)`
- `"triaging"` → `show_progress(entry.phase_label or "Triaging…", entry.phase_step)`
- `"queued"` → `show_placeholder("○ In queue — press Enter to triage now")`
- `"failed"` → `show_placeholder(f"✗ Triage failed:\n\n{entry.failure_reason}")`

---

## Feature 4 — Per-row status coloring

### Goal

Make failed and in-progress rows visually unmissable without reading the status icon.

### Implementation

Add a style lookup to `inbox/widgets.py`:

```python
_ROW_STYLE: dict[Status, str | None] = {
    "failed":   "on dark_red",
    "triaging": "on dark_goldenrod",
    "triaged":  None,
    "queued":   None,
}
```

In `refresh_rows`, apply the style to all cell strings before `add_row`:

```python
style = _ROW_STYLE[row.status]
if style:
    icon, ticket_col, site, when, confidence, summary = (
        f"[{style}]{v}[/]" for v in (icon, f"#{row.ticket_id}", site, when, confidence, summary)
    )
```

This overrides the DataTable zebra stripe for tinted rows. `triaged` and `queued` rows are
unaffected.

---

## Testing

All four features are UI rendering changes. Existing tests in `test_widgets.py` cover
`refresh_rows` and `sort_rows`. New tests needed:

- `test_relative_time` — boundary cases for each time bracket, including the `< 2 min` edge
- `test_confidence_style` — all three values plus an unknown value fallback
- `test_row_style` — verify Rich markup presence in `failed` and `triaging` cells,
  absence in `triaged` and `queued`
- `test_report_pane_modes` — unit test the three `ReportPaneWidget` public methods
  (`show_report`, `show_progress`, `show_placeholder`) via Textual's `Pilot` test runner

No changes to live-API tests. `pipeline.triage_one` callback is tested by asserting the
callback fires with the correct `(label, step)` pairs when a mock Datadog client is injected.

---

## Constraints

- No new runtime dependencies.
- `pipeline.triage_one` signature change is backwards-compatible (`on_phase` defaults to `None`).
- `watcher.run_iteration` and `cli.triage` callers pass no `on_phase` — no changes required there.
- The `ProgressBar` widget is from `textual.widgets`, already a runtime dependency.
