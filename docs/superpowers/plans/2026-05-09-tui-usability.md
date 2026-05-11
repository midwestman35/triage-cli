# TUI Usability Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add four inbox TUI improvements — relative timestamps, color-coded confidence badges, a compound progress bar with phase labels during triage, and per-row background coloring for failed/triaging rows.

**Architecture:** Tasks 1–3 are self-contained to `widgets.py` (pure helpers + markup in `refresh_rows`). Task 4 adds an `on_phase` callback to `pipeline.triage_one`. Task 5 restructures `ReportPaneWidget` from a `Static` leaf into a compound `Widget` with `Static`, `ProgressBar`, and `Label` children. Task 6 wires `app.py` to carry phase events into the widget.

**Tech Stack:** Python 3.11+, Textual (runtime dep), Rich markup (already used throughout), pytest + asyncio.

---

## File Map

| File | Role |
|------|------|
| `triage_cli/inbox/widgets.py` | Add `_relative_time`, `_CONFIDENCE_STYLE`, `_ROW_STYLE`; add `phase_label`/`phase_step` to `RowEntry`; restructure `ReportPaneWidget` |
| `triage_cli/pipeline.py` | Add `on_phase: Callable[[str, int], None] \| None = None` to `triage_one` |
| `triage_cli/inbox/app.py` | Add `_set_phase`; update `_triage_ticket_blocking`, `_run_pipeline_blocking`, `on_mount`, `on_data_table_row_highlighted` |
| `tests/test_widgets.py` | New file — pure unit tests for helper functions |
| `tests/test_pipeline.py` | Add three `on_phase` callback tests |
| `tests/test_inbox_app.py` | Fix broken render assertion; add `_set_phase` test |

---

### Task 1: Relative timestamps

**Files:**
- Create: `tests/test_widgets.py`
- Modify: `triage_cli/inbox/widgets.py`

- [ ] **Step 1: Write failing tests**

Create `tests/test_widgets.py`:

```python
"""Unit tests for inbox/widgets.py helpers."""
from __future__ import annotations

from datetime import UTC, datetime, timedelta

from triage_cli.inbox.widgets import _relative_time


def _now() -> datetime:
    return datetime(2026, 5, 9, 12, 0, 0, tzinfo=UTC)


def test_relative_time_just_now() -> None:
    assert _relative_time(_now() - timedelta(seconds=30), now=_now()) == "just now"


def test_relative_time_boundary_just_now_to_minutes() -> None:
    # 1m59s → "just now"; 2m00s → "Xm ago"
    assert _relative_time(_now() - timedelta(seconds=119), now=_now()) == "just now"
    assert _relative_time(_now() - timedelta(seconds=120), now=_now()) == "2m ago"


def test_relative_time_minutes() -> None:
    assert _relative_time(_now() - timedelta(minutes=14), now=_now()) == "14m ago"


def test_relative_time_hours() -> None:
    assert _relative_time(_now() - timedelta(hours=3), now=_now()) == "3h ago"


def test_relative_time_days() -> None:
    assert _relative_time(_now() - timedelta(days=2), now=_now()) == "2d ago"
```

- [ ] **Step 2: Run to verify failure**

```bash
pytest tests/test_widgets.py -v
```

Expected: `ImportError` — `_relative_time` not defined yet.

- [ ] **Step 3: Implement `_relative_time` and wire into `refresh_rows`**

Add to `triage_cli/inbox/widgets.py` directly after the existing imports block, before `_STATUS_PRIORITY`:

```python
def _relative_time(dt: datetime, *, now: datetime | None = None) -> str:
    _now = now or datetime.now(UTC)
    minutes = int((_now - dt).total_seconds() / 60)
    if minutes < 2:
        return "just now"
    if minutes < 60:
        return f"{minutes}m ago"
    hours = minutes // 60
    if hours < 24:
        return f"{hours}h ago"
    return f"{(_now - dt).days}d ago"
```

In `refresh_rows`, replace:

```python
when = report.generated_at.strftime("%H:%M") if report is not None else "—"
```

with:

```python
when = _relative_time(report.generated_at) if report is not None else "—"
```

- [ ] **Step 4: Run tests**

```bash
pytest tests/test_widgets.py -v
```

Expected: 5 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/test_widgets.py triage_cli/inbox/widgets.py
git commit -m "feat(inbox): relative timestamps in ticket list When column"
```

---

### Task 2: Confidence badges

**Files:**
- Modify: `tests/test_widgets.py`
- Modify: `triage_cli/inbox/widgets.py`

- [ ] **Step 1: Write failing tests**

Append to `tests/test_widgets.py`:

```python
from triage_cli.inbox.widgets import _CONFIDENCE_STYLE


def test_confidence_style_high() -> None:
    assert _CONFIDENCE_STYLE["high"] == "[bold green]high[/]"


def test_confidence_style_medium() -> None:
    assert _CONFIDENCE_STYLE["medium"] == "[yellow]med[/]"


def test_confidence_style_low() -> None:
    assert _CONFIDENCE_STYLE["low"] == "[red]low[/]"


def test_confidence_style_unknown_falls_through() -> None:
    assert _CONFIDENCE_STYLE.get("unexpected", "unexpected") == "unexpected"
```

- [ ] **Step 2: Run to verify failure**

```bash
pytest tests/test_widgets.py::test_confidence_style_high -v
```

Expected: `ImportError` — `_CONFIDENCE_STYLE` not defined.

- [ ] **Step 3: Add `_CONFIDENCE_STYLE` and wire into `refresh_rows`**

In `triage_cli/inbox/widgets.py`, add after `_STATUS_LABELS`:

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
confidence = (
    _CONFIDENCE_STYLE.get(report.confidence, report.confidence)
    if report is not None
    else "—"
)
```

- [ ] **Step 4: Run tests**

```bash
pytest tests/test_widgets.py -v
```

Expected: 9 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/test_widgets.py triage_cli/inbox/widgets.py
git commit -m "feat(inbox): color-coded confidence badges in ticket list"
```

---

### Task 3: Per-row status coloring

**Files:**
- Modify: `tests/test_widgets.py`
- Modify: `triage_cli/inbox/widgets.py`

- [ ] **Step 1: Write failing tests**

Append to `tests/test_widgets.py`:

```python
from triage_cli.inbox.widgets import _ROW_STYLE


def test_row_style_failed_is_dark_red() -> None:
    assert _ROW_STYLE["failed"] == "on dark_red"


def test_row_style_triaging_is_dark_goldenrod() -> None:
    assert _ROW_STYLE["triaging"] == "on dark_goldenrod"


def test_row_style_triaged_is_none() -> None:
    assert _ROW_STYLE["triaged"] is None


def test_row_style_queued_is_none() -> None:
    assert _ROW_STYLE["queued"] is None
```

- [ ] **Step 2: Run to verify failure**

```bash
pytest tests/test_widgets.py::test_row_style_failed_is_dark_red -v
```

Expected: `ImportError` — `_ROW_STYLE` not defined.

- [ ] **Step 3: Add `_ROW_STYLE` and apply in `refresh_rows`**

In `triage_cli/inbox/widgets.py`, add after `_CONFIDENCE_STYLE`:

```python
_ROW_STYLE: dict[Status, str | None] = {
    "failed":   "on dark_red",
    "triaging": "on dark_goldenrod",
    "triaged":  None,
    "queued":   None,
}
```

In `refresh_rows`, after building `icon`, `site`, `when`, `confidence`, and `summary` (and before `self.add_row`), add:

```python
style = _ROW_STYLE[row.status]
if style:
    icon, ticket_col, site, when, confidence, summary = (
        f"[{style}]{v}[/]"
        for v in (icon, f"#{row.ticket_id}", site, when, confidence, summary)
    )
else:
    ticket_col = f"#{row.ticket_id}"
```

Update `self.add_row` to use `ticket_col` in place of the inline `f"#{row.ticket_id}"`:

```python
self.add_row(
    icon,
    ticket_col,
    site,
    when,
    confidence,
    summary,
    key=str(row.ticket_id),
)
```

- [ ] **Step 4: Run all tests**

```bash
pytest tests/test_widgets.py -v
```

Expected: 13 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/test_widgets.py triage_cli/inbox/widgets.py
git commit -m "feat(inbox): per-row background coloring for failed and triaging rows"
```

---

### Task 4: `pipeline.triage_one` phase callback

**Files:**
- Modify: `tests/test_pipeline.py`
- Modify: `triage_cli/pipeline.py`

- [ ] **Step 1: Write failing tests**

Append to `tests/test_pipeline.py`:

```python
def test_triage_one_on_phase_fires_lm_only_when_no_logs(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """With dd_client=None, only step 4 (Asking Claude) fires."""
    from triage_cli.models import LLMTriageOutput

    async def fake_triage(_bundle, model=None, *, verbose=False):  # noqa: ARG001
        return LLMTriageOutput(finding="x", confidence="low", evidence=[], suggested_note="y")

    monkeypatch.setattr(pipeline, "_llm_triage", fake_triage)

    calls: list[tuple[str, int]] = []
    pipeline.triage_one(
        _ticket(), _site(),
        dd_client=None,
        window_minutes=30, levels=["error"], at=None,
        verbose=False, show_spinner=False,
        on_phase=lambda label, step: calls.append((label, step)),
    )
    assert calls == [("Asking Claude", 4)]


def test_triage_one_on_phase_fires_all_steps_with_logs(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """With dd_client and at=None, steps 2, 3, 4 all fire."""
    from triage_cli.models import LLMTriageOutput, LogLine

    async def fake_triage(_bundle, model=None, *, verbose=False):  # noqa: ARG001
        return LLMTriageOutput(finding="x", confidence="low", evidence=[], suggested_note="y")

    async def fake_anchor(_ticket, model=None):  # noqa: ARG001
        return None

    class FakeDD:
        def get_logs(self, _site, _levels, _start, _end):
            ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
            return ([LogLine(timestamp=ts, level="error", message="boom")], False)

    monkeypatch.setattr(pipeline, "_llm_triage", fake_triage)
    monkeypatch.setattr(pipeline, "_llm_extract_anchor", fake_anchor)

    calls: list[tuple[str, int]] = []
    pipeline.triage_one(
        _ticket(), _site(),
        dd_client=FakeDD(),  # type: ignore[arg-type]
        window_minutes=30, levels=["error"], at=None,
        verbose=False, show_spinner=False,
        on_phase=lambda label, step: calls.append((label, step)),
    )
    assert calls == [
        ("Extracting anchor timestamp", 2),
        ("Querying Datadog", 3),
        ("Asking Claude", 4),
    ]


def test_triage_one_on_phase_none_is_safe(monkeypatch: pytest.MonkeyPatch) -> None:
    """on_phase=None (the default) does not raise."""
    from triage_cli.models import LLMTriageOutput

    async def fake_triage(_bundle, model=None, *, verbose=False):  # noqa: ARG001
        return LLMTriageOutput(finding="x", confidence="low", evidence=[], suggested_note="y")

    monkeypatch.setattr(pipeline, "_llm_triage", fake_triage)
    pipeline.triage_one(
        _ticket(), _site(),
        dd_client=None,
        window_minutes=30, levels=["error"], at=None,
        verbose=False, show_spinner=False,
    )  # no on_phase — must not raise
```

- [ ] **Step 2: Run to verify failure**

```bash
pytest tests/test_pipeline.py::test_triage_one_on_phase_fires_lm_only_when_no_logs -v
```

Expected: `TypeError` — `triage_one()` got unexpected keyword argument `on_phase`.

- [ ] **Step 3: Add `on_phase` to `pipeline.triage_one`**

In `triage_cli/pipeline.py`, update the `collections.abc` import (already imports `Iterator`):

```python
from collections.abc import Callable, Iterator
```

Update the `triage_one` signature — add `on_phase` as the last keyword argument:

```python
def triage_one(
    ticket: Ticket,
    site_entry: SiteEntry,
    *,
    dd_client: DatadogClient | None,
    window_minutes: int,
    levels: list[str],
    at: datetime | None,
    verbose: bool,
    show_spinner: bool,
    downloaded_attachments: list | None = None,
    local_files: list | None = None,
    pasted_logs: list | None = None,
    on_phase: Callable[[str, int], None] | None = None,
) -> TriageReport:
```

Add `on_phase` calls at three points inside `triage_one`:

**Before the anchor LLM call** — inside `if dd_client is not None and at is None:`:
```python
    if dd_client is not None and at is None:
        try:
            if on_phase is not None:
                on_phase("Extracting anchor timestamp", 2)
            with spinner("Asking Claude to extract incident timestamp", show=show_spinner):
                extracted_dt = asyncio.run(_llm_extract_anchor(ticket))
```

**Before `dd_client.get_logs`** — inside the `else:` of `if dd_client is None:`:
```python
    else:
        if on_phase is not None:
            on_phase("Querying Datadog", 3)
        with spinner(f"Querying Datadog for {site_entry.site_name}", show=show_spinner):
            log_lines, log_truncated = dd_client.get_logs(
```

**Before `_llm_triage`**:
```python
    if on_phase is not None:
        on_phase("Asking Claude", 4)
    with spinner("Generating triage note", show=show_spinner):
        llm_out = asyncio.run(_llm_triage(bundle, verbose=verbose))
```

Step numbers: 2 = anchor, 3 = Datadog, 4 = LLM. Step 1 ("Fetching ticket") is fired from
`app.py` before calling `triage_one` (wired in Task 6).

- [ ] **Step 4: Run all pipeline tests**

```bash
pytest tests/test_pipeline.py -v
```

Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/test_pipeline.py triage_cli/pipeline.py
git commit -m "feat(pipeline): on_phase callback for triage progress phases"
```

---

### Task 5: `ReportPaneWidget` restructure

**Files:**
- Modify: `tests/test_inbox_app.py` (fix broken assertion first)
- Modify: `triage_cli/inbox/widgets.py`
- Modify: `triage_cli/inbox/app.py` (`on_mount` + `on_data_table_row_highlighted`)

- [ ] **Step 1: Pre-emptively fix the render assertion that will break**

In `tests/test_inbox_app.py`, add `Static` to the import line (currently imports from `triage_cli.inbox.widgets` only):

```python
from textual.widgets import Static
```

In `test_inbox_app_empty_state_subtitle_and_detail_copy`, replace:

```python
detail = app.query_one("#detail", ReportPaneWidget)
assert detail.current_report is None
assert str(detail.render()) == "Select a ticket to view its report."
```

with:

```python
detail = app.query_one("#detail", ReportPaneWidget)
assert detail.current_report is None
report_body = detail.query_one("#report-body", Static)
assert "Select a ticket" in str(report_body.renderable)
```

Run to confirm it currently fails (because `#report-body` doesn't exist yet):

```bash
pytest tests/test_inbox_app.py::test_inbox_app_empty_state_subtitle_and_detail_copy -v
```

Expected: FAIL with `NoMatches` — the restructure in Step 2 will make it pass.

- [ ] **Step 2: Restructure `ReportPaneWidget`**

In `triage_cli/inbox/widgets.py`, update the imports block to add:

```python
from textual.app import ComposeResult
from textual.containers import Vertical
from textual.widget import Widget
from textual.widgets import DataTable, Label, ProgressBar, Static
```

Replace the entire `ReportPaneWidget` class with:

```python
class ReportPaneWidget(Widget):
    """Inbox right pane: report view or triage progress bar."""

    can_focus = True

    DEFAULT_CSS = """
    ReportPaneWidget { padding: 1 2; overflow-y: auto; }
    #progress-region { align: center middle; height: 1fr; }
    #phase-label { margin-bottom: 1; }
    """

    current_report: TriageReport | None = None

    def compose(self) -> ComposeResult:
        yield Static(
            "[dim]Select a ticket to view its report.[/]",
            id="report-body",
        )
        with Vertical(id="progress-region"):
            yield Label("", id="phase-label")
            yield ProgressBar(total=4, show_eta=False, id="phase-bar")

    def on_mount(self) -> None:
        self.query_one("#progress-region").display = False

    def show_report(self, report: TriageReport) -> None:
        self.current_report = report
        self.query_one("#progress-region").display = False
        body = self.query_one("#report-body", Static)
        body.display = True
        body.update(rich_layout(report))

    def show_progress(self, label: str, step: int, total: int = 4) -> None:
        self.current_report = None
        self.query_one("#report-body", Static).display = False
        self.query_one("#progress-region").display = True
        self.query_one("#phase-label", Label).update(label)
        self.query_one("#phase-bar", ProgressBar).update(progress=step)

    def show_placeholder(self, text: str | None = None) -> None:
        self.current_report = None
        self.query_one("#progress-region").display = False
        body = self.query_one("#report-body", Static)
        body.display = True
        body.update(text or "[dim]Select a ticket to view its report.[/]")
```

- [ ] **Step 3: Update `app.py` callers of the old `show()` method**

In `triage_cli/inbox/app.py`, update `on_mount`:

```python
async def on_mount(self) -> None:
    self._hydrate_recent_reports()
    self._refresh_list()
    self.query_one("#detail", ReportPaneWidget).show_placeholder()
    self.query_one("#list", TicketListWidget).focus()
    if self.poll_on_mount:
        self.set_interval(self.opts.interval, self._poll_tick)
        self.run_worker(self._poll_tick(), exclusive=False)
```

Replace `on_data_table_row_highlighted` body:

```python
def on_data_table_row_highlighted(self, event: DataTable.RowHighlighted) -> None:
    row_key = event.row_key.value
    if row_key is None:
        return
    try:
        ticket_id = int(row_key)
    except ValueError:
        return
    entry = self._rows.get(ticket_id)
    detail = self.query_one("#detail", ReportPaneWidget)
    if entry is None:
        detail.show_placeholder()
    elif entry.status == "queued":
        detail.show_placeholder("[dim]○ In queue — press [bold]Enter[/] to triage now[/]")
    elif entry.status == "triaging":
        detail.show_progress(entry.phase_label or "Triaging…", entry.phase_step)
    elif entry.status == "failed":
        reason = entry.failure_reason or "Unknown error"
        detail.show_placeholder(f"[red]✗ Triage failed:[/red]\n\n{reason}")
    else:
        detail.show_report(entry.report)
```

- [ ] **Step 4: Run inbox and widget tests**

```bash
pytest tests/test_inbox_app.py tests/test_widgets.py -v
```

Expected: all tests PASS.

- [ ] **Step 5: Commit**

```bash
git add triage_cli/inbox/widgets.py triage_cli/inbox/app.py tests/test_inbox_app.py
git commit -m "feat(inbox): compound ReportPaneWidget with ProgressBar and phase label"
```

---

### Task 6: App wiring — phase events into the progress bar

**Files:**
- Modify: `triage_cli/inbox/widgets.py` (`RowEntry` phase fields)
- Modify: `triage_cli/inbox/app.py` (`_set_phase`, `_triage_ticket_blocking`, `_run_pipeline_blocking`)
- Modify: `tests/test_inbox_app.py`

- [ ] **Step 1: Add `phase_label` and `phase_step` to `RowEntry`**

In `triage_cli/inbox/widgets.py`, update the `RowEntry` dataclass:

```python
@dataclass
class RowEntry:
    """In-memory state for one ticket in the inbox list."""

    ticket_id: int
    status: Status
    report: TriageReport | None
    site_hint: str | None = None
    failure_reason: str | None = None
    phase_label: str | None = None
    phase_step: int = 0
```

- [ ] **Step 2: Write failing test**

Append to `tests/test_inbox_app.py`:

```python
def test_set_phase_updates_row_entry(tmp_path: Path) -> None:
    """_set_phase stores phase label and step on the RowEntry."""
    from triage_cli.inbox.widgets import RowEntry

    async def run() -> None:
        app = InboxApp(_opts(tmp_path), notes_dir=tmp_path, poll_on_mount=False)
        async with app.run_test():
            app._rows[999] = RowEntry(ticket_id=999, status="triaging", report=None)
            app._set_phase(999, "Asking Claude", 4)
            await asyncio.sleep(0)

            entry = app._rows[999]
            assert entry.phase_label == "Asking Claude"
            assert entry.phase_step == 4

    asyncio.run(run())
```

- [ ] **Step 3: Run to verify failure**

```bash
pytest tests/test_inbox_app.py::test_set_phase_updates_row_entry -v
```

Expected: `AttributeError` — `InboxApp` has no attribute `_set_phase`.

- [ ] **Step 4: Add `_set_phase` to `InboxApp`**

In `triage_cli/inbox/app.py`, add after `_set_failure`:

```python
def _set_phase(self, ticket_id: int, label: str, step: int) -> None:
    entry = self._rows.get(ticket_id)
    if entry is None:
        return
    entry.phase_label = label
    entry.phase_step = step
    if self._selected_ticket_id() == ticket_id:
        self.query_one("#detail", ReportPaneWidget).show_progress(label, step)
```

- [ ] **Step 5: Wire phase events in `_triage_ticket_blocking` and `_run_pipeline_blocking`**

In `_triage_ticket_blocking`, add the step 1 call immediately after the existing
`self.call_from_thread(self._set_status, ticket_id, "triaging")` line:

```python
self.call_from_thread(self._set_status, ticket_id, "triaging")
self.call_from_thread(self._set_phase, ticket_id, "Fetching ticket", 1)
```

Replace `_run_pipeline_blocking` with:

```python
def _run_pipeline_blocking(
    self, ticket_id: int, ticket: Ticket, site_entry: SiteEntry
) -> None:
    """Run the triage pipeline for a resolved ticket+site (worker thread)."""
    def _phase(label: str, step: int) -> None:
        self.call_from_thread(self._set_phase, ticket_id, label, step)

    try:
        if self.opts.no_logs:
            report = pipeline.triage_one(
                ticket, site_entry, dd_client=None,
                window_minutes=self.opts.window_minutes,
                levels=self.opts.levels, at=None,
                verbose=self.opts.verbose, show_spinner=False,
                on_phase=_phase,
            )
        else:
            with DatadogClient.from_env() as dd:
                report = pipeline.triage_one(
                    ticket, site_entry, dd_client=dd,
                    window_minutes=self.opts.window_minutes,
                    levels=self.opts.levels, at=None,
                    verbose=self.opts.verbose, show_spinner=False,
                    on_phase=_phase,
                )
        render.save_note(report, ticket_id)
        self._on_complete(report)
    except (RuntimeError, ValueError) as e:
        self._on_failure(ticket_id, str(e))
```

- [ ] **Step 6: Run full test suite**

```bash
pytest -v
```

Expected: all tests PASS. Note the count — if any regressions appear in
`test_inbox_app.py` or `test_pipeline.py`, check that the old `show()` method has
been fully replaced by `show_report` / `show_progress` / `show_placeholder`.

- [ ] **Step 7: Commit**

```bash
git add triage_cli/inbox/widgets.py triage_cli/inbox/app.py tests/test_inbox_app.py
git commit -m "feat(inbox): wire on_phase into progress bar from triage pipeline"
```
