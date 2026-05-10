# Final Phase Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship triage-cli's three scope-locked 1.0 features per `docs/superpowers/specs/2026-05-10-final-phase-design.md` (commit `31ba3f4`): PII redactor at the LLM boundary, token-aware context builder, and inbox design tokens + density toggle.

**Architecture:**
- Feature 1: A new `triage_cli/redact.py` module is invoked from inside `llm.py`'s three call sites (`triage`, `extract_anchor`, `extract_site`). Single trust point — every consumer is protected automatically.
- Feature 2: A new `triage_cli/context.py` module returns a trimmed list of `LogLine` plus a `ContextSummary`. Called from `pipeline.triage_one`; the bundle stores the trimmed list and the existing rendering path is unchanged.
- Feature 3: Inline `DEFAULT_CSS` strings are extracted into a single `inbox.tcss` file with named tokens (organizational refactor — *not* a theme system). Density toggle is a new `'d'` keybinding persisted in the existing watcher state file via a v1→v2 migration.

**Tech Stack:** Python 3.11+, Pydantic v2, pytest, Textual ≥0.80, Claude Agent SDK, `re` (stdlib only — no new runtime deps).

**Build sequence:** Phase 1 (redactor) → Phase 2 (context builder) → Phase 3 (inbox). Each phase commits incrementally; phases can be merged independently.

**Spec deviations:** One — `build_log_section` returns `(list[LogLine], ContextSummary)` instead of `(str, ContextSummary)`. Reason: keeps `TriageBundle.as_user_message`'s existing rendering path intact; avoids duplicating render logic in the context module. Behavior identical.

---

## Phase 1 — PII Redactor

### Task 1.1: Redactor module skeleton + phone detection (TDD)

**Files:**
- Create: `triage_cli/redact.py`
- Test: `tests/test_redact.py`

- [ ] **Step 1: Write the failing tests for phone detection**

```python
# tests/test_redact.py
"""Tests for triage_cli.redact (PII redactor at the LLM boundary)."""
from triage_cli.redact import RedactionCounts, redact


def test_redacts_simple_phone() -> None:
    out, counts = redact("Call 555-123-4567 for status.")
    assert out == "Call <PHONE> for status."
    assert counts.phones == 1


def test_redacts_phone_with_parens_and_country_code() -> None:
    out, counts = redact("Reach me at +1 (555) 123-4567 today.")
    assert "<PHONE>" in out
    assert counts.phones == 1


def test_redacts_phone_with_dots() -> None:
    out, counts = redact("Number: 555.123.4567 confirmed.")
    assert "<PHONE>" in out
    assert counts.phones == 1


def test_redacts_bare_ten_digits() -> None:
    out, counts = redact("Phone 5551234567 logged.")
    assert "<PHONE>" in out
    assert counts.phones == 1


def test_redacts_multiple_phones() -> None:
    _out, counts = redact("Try 555-111-2222 or 555-333-4444.")
    assert counts.phones == 2


def test_does_not_redact_inside_alphanumeric_id() -> None:
    # Operational IDs that contain digits but aren't phones
    out, counts = redact("Call-ID: abc5551234567xyz@host")
    assert "<PHONE>" not in out
    assert counts.phones == 0


def test_does_not_redact_short_number_sequences() -> None:
    out, counts = redact("Status code 200 with 5 retries.")
    assert "<PHONE>" not in out
    assert counts.phones == 0


def test_counts_default_to_zero() -> None:
    out, counts = redact("No PII here at all.")
    assert out == "No PII here at all."
    assert counts.phones == 0
    assert counts.addresses == 0
    assert counts.coords == 0
    assert counts.enabled is True
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `pytest tests/test_redact.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'triage_cli.redact'`.

- [ ] **Step 3: Implement the minimal module**

```python
# triage_cli/redact.py
"""PII redactor applied at the LLM boundary.

Scope (locked by spec 2026-05-10-final-phase-design.md):
- Caller PII only: phones, addresses, GPS coords.
- Names: explicit gap (regex unreliable; revisit only if compliance asks).
- Operational IDs (Call-IDs, ticket #s, station codes, CNCs, sites): preserved.
"""
from __future__ import annotations

import re

from pydantic import BaseModel

# Phone: optional +1, common separators, with negative lookarounds
# preventing matches inside alphanumeric tokens like "abc5551234567xyz".
_PHONE_PATTERN = re.compile(
    r"(?<![A-Za-z0-9])"
    r"(?:\+?1[-.\s]?)?\(?\d{3}\)?[-.\s]?\d{3}[-.\s]?\d{4}"
    r"(?![A-Za-z0-9])"
)


class RedactionCounts(BaseModel):
    """Per-call redaction tally surfaced via verbose stderr and saved JSON."""

    phones: int = 0
    addresses: int = 0
    coords: int = 0
    enabled: bool = True


def _is_pre_redacted(match: str) -> bool:
    """Skip values that are already redacted (e.g., '***-***-1234' from Zendesk)."""
    s = match.lower()
    return "***" in s or "xxx" in s or "[redacted]" in s


def redact(text: str) -> tuple[str, RedactionCounts]:
    """Redact caller PII from ``text``. Returns (redacted_text, counts)."""
    counts = RedactionCounts(enabled=True)

    def _sub_phone(m: re.Match[str]) -> str:
        if _is_pre_redacted(m.group(0)):
            return m.group(0)
        counts.phones += 1
        return "<PHONE>"

    text = _PHONE_PATTERN.sub(_sub_phone, text)
    return text, counts
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `pytest tests/test_redact.py -v`
Expected: PASS — all 8 tests pass.

- [ ] **Step 5: Commit**

```bash
git add triage_cli/redact.py tests/test_redact.py
git commit -m "feat(redact): phone redaction with operational-ID guards"
```

---

### Task 1.2: Address detection (street-line only, per Q7 default)

**Files:**
- Modify: `triage_cli/redact.py`
- Modify: `tests/test_redact.py`

- [ ] **Step 1: Add failing tests for street-line addresses**

Append to `tests/test_redact.py`:

```python
def test_redacts_simple_street() -> None:
    out, counts = redact("Caller at 123 Main St reported the outage.")
    assert "<ADDR>" in out
    assert "Main St" not in out
    assert counts.addresses == 1


def test_redacts_multi_word_street() -> None:
    out, counts = redact("Address: 4500 Old Mill Road confirmed.")
    assert "<ADDR>" in out
    assert counts.addresses == 1


def test_redacts_with_avenue_and_other_suffixes() -> None:
    for s in ("789 Oak Ave", "12 First Boulevard", "1 Court Place", "55 Highway", "9 Circle"):
        out, counts = redact(f"Caller at {s} confirmed.")
        assert "<ADDR>" in out, f"failed for {s!r}"
        assert counts.addresses == 1


def test_does_not_redact_city_state_zip_when_no_street() -> None:
    # Q7 default: street-line only; city/state survive for site extraction.
    out, counts = redact("Site is in Springfield, IL 62701.")
    assert "<ADDR>" not in out
    assert "Springfield" in out
    assert counts.addresses == 0


def test_redacts_street_but_preserves_following_city() -> None:
    # The street portion is redacted; the city following the comma is kept.
    out, counts = redact("Caller at 123 Main St, Springfield IL 62701.")
    assert "<ADDR>" in out
    assert "Springfield" in out
    assert counts.addresses == 1
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_redact.py -v`
Expected: 5 new tests fail; existing tests still pass.

- [ ] **Step 3: Add address pattern + sub function**

In `triage_cli/redact.py`, add below `_PHONE_PATTERN`:

```python
_STREET_SUFFIXES = (
    r"St|Street|Ave|Avenue|Rd|Road|Blvd|Boulevard|Ln|Lane|Dr|Drive|"
    r"Ct|Court|Way|Pl|Place|Hwy|Highway|Pkwy|Parkway|Ter|Terrace|Cir|Circle"
)
# Street-line only: <number> <Capitalized words> <suffix>. Stops at the first
# comma so the city/state/ZIP that follow remain intact for site extraction
# (Q7 default in the spec).
_ADDRESS_PATTERN = re.compile(
    rf"\b\d{{1,6}}\s+(?:[A-Z][A-Za-z'-]*\s+){{1,4}}(?:{_STREET_SUFFIXES})\.?\b",
)
```

In `redact()`, add below the phone substitution:

```python
    def _sub_address(m: re.Match[str]) -> str:
        if _is_pre_redacted(m.group(0)):
            return m.group(0)
        counts.addresses += 1
        return "<ADDR>"

    text = _ADDRESS_PATTERN.sub(_sub_address, text)
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_redact.py -v`
Expected: all tests pass (13 total).

- [ ] **Step 5: Commit**

```bash
git add triage_cli/redact.py tests/test_redact.py
git commit -m "feat(redact): street-line address redaction (preserves city for site lookup)"
```

---

### Task 1.3: GPS coordinate detection

**Files:**
- Modify: `triage_cli/redact.py`
- Modify: `tests/test_redact.py`

- [ ] **Step 1: Add failing tests for coords**

Append to `tests/test_redact.py`:

```python
def test_redacts_decimal_coords_comma() -> None:
    out, counts = redact("Caller at 33.7490, -84.3880 reported.")
    assert "<COORDS>" in out
    assert counts.coords == 1


def test_redacts_decimal_coords_space() -> None:
    out, counts = redact("Coords: 40.7128 -74.0060 confirmed.")
    assert "<COORDS>" in out
    assert counts.coords == 1


def test_does_not_redact_low_precision_pairs() -> None:
    # Version numbers, prices, etc. — require 4+ decimals.
    out, counts = redact("Version 1.23, build 4.56 deployed.")
    assert "<COORDS>" not in out
    assert counts.coords == 0


def test_redacts_multiple_coord_pairs() -> None:
    _out, counts = redact("Pings: 33.7490, -84.3880 then 40.7128, -74.0060.")
    assert counts.coords == 2
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_redact.py -v`
Expected: 4 new tests fail.

- [ ] **Step 3: Add coord pattern + sub function**

In `triage_cli/redact.py`, add below `_ADDRESS_PATTERN`:

```python
# Lat,lon decimal pairs with 4+ decimals to avoid matching version numbers.
_COORD_PATTERN = re.compile(
    r"-?\d{1,2}\.\d{4,}\s*[,;\s]\s*-?\d{1,3}\.\d{4,}"
)
```

In `redact()`, add below the address substitution:

```python
    def _sub_coord(m: re.Match[str]) -> str:
        if _is_pre_redacted(m.group(0)):
            return m.group(0)
        counts.coords += 1
        return "<COORDS>"

    text = _COORD_PATTERN.sub(_sub_coord, text)
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_redact.py -v`
Expected: all 17 tests pass.

- [ ] **Step 5: Commit**

```bash
git add triage_cli/redact.py tests/test_redact.py
git commit -m "feat(redact): GPS coord redaction with 4-decimal precision floor"
```

---

### Task 1.4: Pass-through guard verification (covered by `_is_pre_redacted`)

**Files:**
- Modify: `tests/test_redact.py`

- [ ] **Step 1: Add explicit pass-through tests**

Append to `tests/test_redact.py`:

```python
def test_pass_through_already_starred_phone() -> None:
    out, counts = redact("Pre-redacted: ***-***-1234 in ticket.")
    assert "***-***-1234" in out
    # The pattern won't match this anyway because of the asterisks, but
    # the guard is the safety net for borderline cases.
    assert counts.phones == 0


def test_pass_through_redacted_marker_in_text() -> None:
    out, _counts = redact("Address [REDACTED] by Zendesk admin.")
    assert "[REDACTED]" in out
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `pytest tests/test_redact.py -v`
Expected: all tests pass — `_is_pre_redacted` already covers these.

- [ ] **Step 3: Commit**

```bash
git add tests/test_redact.py
git commit -m "test(redact): explicit pass-through tests for already-redacted values"
```

---

### Task 1.5: Add `RedactionCounts` to `TriageReport` model

**Files:**
- Modify: `triage_cli/models.py`
- Modify: `tests/test_models.py`

- [ ] **Step 1: Add failing test in `tests/test_models.py`**

Append a test that constructs a `TriageReport` with and without `redaction_summary`:

```python
def test_triage_report_accepts_optional_redaction_summary() -> None:
    from triage_cli.models import TriageReport, TimeWindow
    from triage_cli.redact import RedactionCounts
    from datetime import datetime, UTC

    report = TriageReport(
        finding="x",
        confidence="medium",
        evidence=[],
        suggested_note="x",
        ticket_id=1,
        site_name="us-ga-roswell",
        window=TimeWindow(start=datetime.now(UTC), end=datetime.now(UTC)),
        sources=["zendesk"],
        log_event_count=0,
        generated_at=datetime.now(UTC),
        redaction_summary=RedactionCounts(phones=2, addresses=1, coords=0, enabled=True),
    )
    assert report.redaction_summary is not None
    assert report.redaction_summary.phones == 2

    # Default to None for backwards compatibility
    report2 = TriageReport(
        finding="x",
        confidence="medium",
        evidence=[],
        suggested_note="x",
        ticket_id=1,
        site_name="us-ga-roswell",
        window=TimeWindow(start=datetime.now(UTC), end=datetime.now(UTC)),
        sources=["zendesk"],
        log_event_count=0,
        generated_at=datetime.now(UTC),
    )
    assert report2.redaction_summary is None
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pytest tests/test_models.py::test_triage_report_accepts_optional_redaction_summary -v`
Expected: FAIL — `TypeError: TriageReport.__init__() got unexpected keyword argument 'redaction_summary'`.

- [ ] **Step 3: Add the field to `TriageReport`**

In `triage_cli/models.py`, locate `class TriageReport` (currently at line ~330). Add an import at the top of the file:

```python
from triage_cli.redact import RedactionCounts
```

(If a circular-import warning appears at runtime, move the import inside `if TYPE_CHECKING:` and quote the field annotation.)

Add the field to `TriageReport`:

```python
class TriageReport(LLMTriageOutput):
    """Full triage report: LLM output + pipeline-derived metadata."""

    ticket_id: int
    site_name: str
    window: TimeWindow
    sources: list[str]
    log_event_count: int
    generated_at: datetime
    redaction_summary: RedactionCounts | None = None  # <-- new

    @field_validator("generated_at")
    ...
```

- [ ] **Step 4: Run test to verify it passes**

Run: `pytest tests/test_models.py -v`
Expected: PASS, no regressions.

- [ ] **Step 5: Commit**

```bash
git add triage_cli/models.py tests/test_models.py
git commit -m "feat(models): optional redaction_summary on TriageReport"
```

---

### Task 1.6: Wire redactor into `llm.py`

**Files:**
- Modify: `triage_cli/llm.py`
- Modify: `tests/test_llm.py`

- [ ] **Step 1: Read `triage_cli/llm.py` end-to-end** to identify the three call sites: `triage`, `extract_anchor`, `extract_site`. Each receives ticket-derived text that's interpolated into a prompt before calling `_collect_text(...)`.

- [ ] **Step 2: Add a failing test asserting redaction is applied**

Append to `tests/test_llm.py`:

```python
def test_triage_redacts_phone_in_ticket_before_llm(monkeypatch) -> None:
    """Verify that ticket text is redacted before being sent to Claude."""
    captured: dict[str, str] = {}

    async def fake_collect(prompt: str, system_prompt: str, model: str) -> str:
        captured["prompt"] = prompt
        return '{"finding": "x", "confidence": "low", "evidence": [], "suggested_note": "x"}'

    from triage_cli import llm
    from triage_cli.models import (
        AnchorSource, SiteEntry, Ticket, TriageBundle,
    )
    from datetime import datetime, UTC
    import asyncio

    monkeypatch.setattr(llm, "_collect_text", fake_collect)

    ticket = Ticket(
        id=1,
        subject="Outage",
        description="Caller 555-123-4567 reported issue.",
        requester_org="Acme",
        created_at=datetime.now(UTC),
        comments=[],
    )
    bundle = TriageBundle(
        ticket=ticket,
        site_entry=SiteEntry(friendly_name="Acme", site_name="acme", cnc="abc"),
        log_lines=[],
        log_truncated=False,
        anchor=datetime.now(UTC),
        anchor_source=AnchorSource.CREATED_AT,
        window_start=datetime.now(UTC),
        window_end=datetime.now(UTC),
    )

    asyncio.run(llm.triage(bundle, verbose=False))

    assert "555-123-4567" not in captured["prompt"]
    assert "<PHONE>" in captured["prompt"]


def test_triage_no_redact_kwarg_passes_raw(monkeypatch) -> None:
    captured: dict[str, str] = {}

    async def fake_collect(prompt: str, system_prompt: str, model: str) -> str:
        captured["prompt"] = prompt
        return '{"finding": "x", "confidence": "low", "evidence": [], "suggested_note": "x"}'

    from triage_cli import llm
    from triage_cli.models import (
        AnchorSource, SiteEntry, Ticket, TriageBundle,
    )
    from datetime import datetime, UTC
    import asyncio

    monkeypatch.setattr(llm, "_collect_text", fake_collect)

    ticket = Ticket(
        id=1,
        subject="Outage",
        description="Caller 555-123-4567 reported issue.",
        requester_org="Acme",
        created_at=datetime.now(UTC),
        comments=[],
    )
    bundle = TriageBundle(
        ticket=ticket,
        site_entry=SiteEntry(friendly_name="Acme", site_name="acme", cnc="abc"),
        log_lines=[],
        log_truncated=False,
        anchor=datetime.now(UTC),
        anchor_source=AnchorSource.CREATED_AT,
        window_start=datetime.now(UTC),
        window_end=datetime.now(UTC),
    )

    asyncio.run(llm.triage(bundle, verbose=False, redact_enabled=False))

    assert "555-123-4567" in captured["prompt"]
    assert "<PHONE>" not in captured["prompt"]
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `pytest tests/test_llm.py::test_triage_redacts_phone_in_ticket_before_llm tests/test_llm.py::test_triage_no_redact_kwarg_passes_raw -v`
Expected: FAIL — phone number still in prompt; `redact_enabled` is not a recognized kwarg.

- [ ] **Step 4: Wire redaction into all three LLM-touching functions**

In `triage_cli/llm.py`:

1. Add at the top:

```python
from triage_cli.redact import RedactionCounts, redact
```

2. Add a private helper near the existing `_collect_text`:

```python
def _maybe_redact(text: str, *, enabled: bool) -> tuple[str, RedactionCounts]:
    """Redact when enabled; pass-through with disabled counts when not."""
    if not enabled:
        return text, RedactionCounts(enabled=False)
    return redact(text)
```

3. For `async def triage(bundle, *, model=None, verbose=False)` — change signature to:

```python
async def triage(
    bundle: TriageBundle,
    *,
    model: str | None = None,
    verbose: bool = False,
    redact_enabled: bool = True,
) -> LLMTriageOutput:
    ...
    user_message = bundle.as_user_message()
    user_message, counts = _maybe_redact(user_message, enabled=redact_enabled)
    if verbose:
        if counts.enabled:
            print(
                f"redacted: {counts.phones} phones, {counts.addresses} addresses, "
                f"{counts.coords} coords",
                file=sys.stderr,
            )
        else:
            print("redaction: disabled", file=sys.stderr)
    # existing _collect_text(...) call uses user_message
```

(Note: `print(..., file=sys.stderr)` requires `import sys` at the top — confirm it's already there.)

Same shape for `extract_anchor(ticket, *, model=None, redact_enabled=True)` and `extract_site(ticket, sites, *, model=None, redact_enabled=True)`. Each should pass through `_maybe_redact` on whatever text it interpolates into the prompt. For functions that don't currently take a `verbose` kwarg, add it; default `False`.

The "redaction: disabled" stderr line must be printed **regardless** of verbose, per the spec — adjust the conditional.

- [ ] **Step 5: Run tests to verify they pass**

Run: `pytest tests/test_llm.py -v`
Expected: all tests pass; existing tests not broken.

- [ ] **Step 6: Commit**

```bash
git add triage_cli/llm.py tests/test_llm.py
git commit -m "feat(llm): redact PII at the LLM boundary (triage/anchor/site)"
```

---

### Task 1.7: `--no-redact` CLI flag wired through

**Files:**
- Modify: `triage_cli/cli.py`
- Modify: `triage_cli/pipeline.py`
- Modify: `tests/test_cli.py`

- [ ] **Step 1: Add a failing test asserting `--no-redact` propagates**

Append to `tests/test_cli.py`:

```python
def test_triage_passes_no_redact_flag(monkeypatch, tmp_path) -> None:
    """`--no-redact` should reach pipeline.triage_one as redact_enabled=False."""
    from triage_cli import cli, pipeline
    from typer.testing import CliRunner

    captured: dict[str, object] = {}

    def fake_triage_one(*args, **kwargs):
        captured["redact_enabled"] = kwargs.get("redact_enabled")
        # Build a minimal valid TriageReport for the renderer
        from triage_cli.models import TriageReport, TimeWindow
        from datetime import datetime, UTC
        return TriageReport(
            finding="x", confidence="low", evidence=[], suggested_note="x",
            ticket_id=1, site_name="acme",
            window=TimeWindow(start=datetime.now(UTC), end=datetime.now(UTC)),
            sources=["zendesk"], log_event_count=0,
            generated_at=datetime.now(UTC),
        )

    # Stub everything between CLI parse and pipeline call.
    # ... (set environment, mock zendesk client, mock site map load) ...
    # Invoke runner with --no-redact and assert captured["redact_enabled"] is False.
```

(The exact stubbing follows the patterns already in `tests/test_cli.py`. If the CLI test harness in this repo is heavier than this sketch, prefer adding a unit test directly against the pipeline's option-passing instead.)

- [ ] **Step 2: Run test to verify it fails**

Run: `pytest tests/test_cli.py -v -k no_redact`
Expected: FAIL — flag doesn't exist yet.

- [ ] **Step 3: Add `--no-redact` option to each CLI command**

In `triage_cli/cli.py`, find the `triage`, `investigate`, `watch`, and `inbox` typer commands. Add to each (alongside the existing options):

```python
no_redact: bool = typer.Option(
    False, "--no-redact",
    help="Disable PII redaction before sending content to the LLM. "
         "Default is on; use this for debugging or certified test runs.",
),
```

Thread `redact_enabled = not no_redact` into the call into `pipeline.triage_one(...)` (or watcher options for `watch`/`inbox`).

- [ ] **Step 4: Add `redact_enabled` parameter to `pipeline.triage_one`**

In `triage_cli/pipeline.py`, add to `triage_one`'s signature:

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
    redact_enabled: bool = True,  # <-- new
) -> TriageReport:
```

Pass `redact_enabled=redact_enabled, verbose=verbose` into both `_llm_extract_anchor(...)` and `_llm_triage(...)` calls. (And into `_llm_extract_site` from `resolve_site` — which means `resolve_site` also needs the kwarg; thread it through.)

For `WatcherOptions` (used by `watch` and `inbox`), add `redact_enabled: bool = True` as a field and read it in the iteration loop where `triage_one` is called.

- [ ] **Step 5: Run tests to verify they pass**

Run: `pytest -v`
Expected: full suite passes.

- [ ] **Step 6: Commit**

```bash
git add triage_cli/cli.py triage_cli/pipeline.py triage_cli/watcher.py tests/test_cli.py
git commit -m "feat(cli): --no-redact flag threaded through triage/investigate/watch/inbox"
```

---

### Task 1.8: Surface `redaction_summary` on `TriageReport` from the pipeline

**Files:**
- Modify: `triage_cli/llm.py`
- Modify: `triage_cli/pipeline.py`
- Modify: `tests/test_pipeline.py`

- [ ] **Step 1: Add a failing test for the report field**

Append to `tests/test_pipeline.py`:

```python
def test_triage_report_includes_redaction_summary(monkeypatch) -> None:
    """When redaction is enabled, the report's redaction_summary must reflect counts."""
    from datetime import UTC, datetime
    from triage_cli import pipeline
    from triage_cli.models import (
        AnchorSource, LLMTriageOutput, SiteEntry, Ticket, TriageBundle,
    )
    from triage_cli.redact import RedactionCounts

    async def fake_triage(bundle, *, model=None, verbose=False, redact_enabled=True):
        return (
            LLMTriageOutput(finding="x", confidence="low", evidence=[], suggested_note="x"),
            RedactionCounts(phones=2, addresses=1, coords=0, enabled=True),
        )

    async def fake_extract_anchor(ticket, *, model=None, redact_enabled=True):
        return None

    monkeypatch.setattr(pipeline, "_llm_triage", fake_triage)
    monkeypatch.setattr(pipeline, "_llm_extract_anchor", fake_extract_anchor)

    ticket = Ticket(
        id=1, subject="x", description="x", requester_org="Acme",
        created_at=datetime.now(UTC), comments=[],
    )
    site = SiteEntry(friendly_name="Acme", site_name="acme", cnc="abc")

    report = pipeline.triage_one(
        ticket, site,
        dd_client=None, window_minutes=15, levels=["error", "warn"],
        at=None, verbose=False, show_spinner=False,
    )
    assert report.redaction_summary is not None
    assert report.redaction_summary.phones == 2
    assert report.redaction_summary.addresses == 1
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pytest tests/test_pipeline.py -v -k redaction_summary`
Expected: FAIL.

- [ ] **Step 3: Make `_llm_triage` return counts alongside the output**

In `triage_cli/llm.py`, change `triage()`'s return type:

```python
async def triage(
    bundle: TriageBundle,
    *,
    model: str | None = None,
    verbose: bool = False,
    redact_enabled: bool = True,
) -> tuple[LLMTriageOutput, RedactionCounts]:
    ...
    return llm_output, counts
```

- [ ] **Step 4: Update `pipeline.triage_one` to attach counts**

In `triage_cli/pipeline.py`, replace:

```python
with spinner("Generating triage note", show=show_spinner):
    llm_out = asyncio.run(_llm_triage(bundle, verbose=verbose))
```

with:

```python
with spinner("Generating triage note", show=show_spinner):
    llm_out, redaction_counts = asyncio.run(
        _llm_triage(bundle, verbose=verbose, redact_enabled=redact_enabled)
    )
```

Add to the `TriageReport(...)` constructor at the end:

```python
return TriageReport(
    **llm_out.model_dump(),
    ticket_id=ticket.id,
    site_name=site_entry.site_name,
    window=TimeWindow(start=start, end=end),
    sources=sources,
    log_event_count=len(log_lines),
    generated_at=datetime.now(UTC),
    redaction_summary=redaction_counts,  # <-- new
)
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `pytest -v`
Expected: full suite passes.

- [ ] **Step 6: Commit**

```bash
git add triage_cli/llm.py triage_cli/pipeline.py tests/test_pipeline.py
git commit -m "feat(pipeline): attach redaction_summary to TriageReport"
```

---

### Task 1.9: Documentation for Feature 1

**Files:**
- Modify: `README.md`
- Modify: `docs/runbooks/04-troubleshooting.md`
- Modify: `docs/CHEATSHEET.md`
- Modify: `CLAUDE.md`

- [ ] **Step 1: README — document `--no-redact`**

Add to the flags table for each command (or wherever flags are listed): `--no-redact` with the help text from Task 1.7. Add a short subsection: "By default, caller PII (phone numbers, street addresses, GPS coords) is replaced with `<PHONE>`/`<ADDR>`/`<COORDS>` placeholders before any text is sent to Claude. Use `--no-redact` to disable for debugging."

- [ ] **Step 2: 04-troubleshooting.md — false-positive notes**

Add a "Redactor" section: "If a triage note references `<PHONE>` or `<ADDR>` where the original ticket had operational data (e.g., a long station ID matched the phone regex), re-run with `--no-redact` to confirm. Open an issue with the offending input."

- [ ] **Step 3: CHEATSHEET.md — list the flag**

Add `--no-redact` to the flags reference.

- [ ] **Step 4: CLAUDE.md — refine the LLM-access section**

Find the existing line: "Internal Zendesk comments **are** sent to the LLM. v1 is terminal-only so this is acceptable; if anything ever posts back to Zendesk, this assumption must be revisited."

Replace with:

> Internal Zendesk comments **are** sent to the LLM (comment text itself, including employee notes). Caller PII (phone numbers, street-line addresses, GPS coords) is **redacted** at the LLM boundary by default — see `triage_cli/redact.py`. The post-back risk is reduced but not eliminated; if anything ever posts back to Zendesk, this assumption must be revisited.

- [ ] **Step 5: Commit**

```bash
git add README.md docs/runbooks/04-troubleshooting.md docs/CHEATSHEET.md CLAUDE.md
git commit -m "docs: document --no-redact and refine LLM-access caveat"
```

---

## Phase 2 — Token-Aware Context Builder

### Task 2.1: `ContextSummary` model + `estimate_tokens` helper (TDD)

**Files:**
- Create: `triage_cli/context.py`
- Create: `tests/test_context.py`

- [ ] **Step 1: Write failing tests**

```python
# tests/test_context.py
"""Tests for triage_cli.context (token-aware log selection)."""
from triage_cli.context import ContextSummary, estimate_tokens


def test_estimate_tokens_is_chars_over_four() -> None:
    assert estimate_tokens("") == 0
    assert estimate_tokens("a" * 4) == 1
    assert estimate_tokens("a" * 100) == 25


def test_context_summary_fields() -> None:
    s = ContextSummary(candidates=200, kept=47, budget_tokens=6000, used_tokens=5921)
    assert s.candidates == 200
    assert s.kept == 47
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_context.py -v`
Expected: FAIL — module missing.

- [ ] **Step 3: Implement skeleton**

```python
# triage_cli/context.py
"""Token-aware log selection for the triage prompt.

Spec 2026-05-10-final-phase-design.md, Feature 2.

Selection is deterministic: each line is scored by severity, subject-token
match, anchor proximity, and a dedupe penalty. The top-N lines that fit
within ``budget`` tokens are kept; selection is then re-sorted chronologically
for the prompt.
"""
from __future__ import annotations

from pydantic import BaseModel


class ContextSummary(BaseModel):
    """Audit summary of the log-selection step (attached to TriageReport)."""

    candidates: int
    kept: int
    budget_tokens: int
    used_tokens: int


def estimate_tokens(text: str) -> int:
    """Approximate tokens as ``len(text) // 4`` (no tokenizer dep, by design)."""
    return len(text) // 4
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_context.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add triage_cli/context.py tests/test_context.py
git commit -m "feat(context): ContextSummary model and token estimator"
```

---

### Task 2.2: `extract_subject_tokens`

**Files:**
- Modify: `triage_cli/context.py`
- Modify: `tests/test_context.py`

- [ ] **Step 1: Write failing tests**

Append to `tests/test_context.py`:

```python
from triage_cli.context import extract_subject_tokens


def test_extract_subject_tokens_lowercases() -> None:
    assert extract_subject_tokens("SIP TIMEOUT issue") == ["timeout", "issue"]


def test_extract_subject_tokens_drops_short() -> None:
    # Tokens < 4 chars are dropped.
    assert "sip" not in extract_subject_tokens("sip outage in roswell")


def test_extract_subject_tokens_drops_stopwords() -> None:
    tokens = extract_subject_tokens("The system has problem with the network")
    assert "the" not in tokens
    assert "with" not in tokens
    assert "network" in tokens


def test_extract_subject_tokens_dedupes() -> None:
    tokens = extract_subject_tokens("network network issue with network")
    assert tokens.count("network") == 1
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_context.py -v`
Expected: FAIL — function missing.

- [ ] **Step 3: Implement**

In `triage_cli/context.py`, add:

```python
import re

_STOPWORDS = frozenset({
    "the", "and", "for", "with", "from", "this", "that", "has", "have",
    "was", "were", "are", "you", "your", "our", "their", "but", "not",
    "all", "can", "any", "had", "her", "his", "she", "they", "ticket",
    "issue", "problem", "report", "reported", "into", "onto", "over",
    "under", "about", "after", "before", "while",
})


def extract_subject_tokens(subject: str) -> list[str]:
    """Lowercase, dedupe, drop stopwords and tokens shorter than 4 chars."""
    seen: set[str] = set()
    out: list[str] = []
    for tok in re.findall(r"\b[a-zA-Z]{4,}\b", subject.lower()):
        if tok in _STOPWORDS or tok in seen:
            continue
        seen.add(tok)
        out.append(tok)
    return out
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_context.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add triage_cli/context.py tests/test_context.py
git commit -m "feat(context): extract_subject_tokens with stopword filter"
```

---

### Task 2.3: `score_log_line` (severity + subject + proximity + dedupe)

**Files:**
- Modify: `triage_cli/context.py`
- Modify: `tests/test_context.py`

- [ ] **Step 1: Write failing tests**

Append to `tests/test_context.py`:

```python
from datetime import UTC, datetime, timedelta

from triage_cli.context import score_log_line
from triage_cli.models import LogLine


def _line(level: str, msg: str, ts: datetime | None = None) -> LogLine:
    return LogLine(timestamp=ts or datetime.now(UTC), level=level, message=msg)


def test_score_severity_weights() -> None:
    anchor = datetime(2026, 5, 10, 12, 0, 0, tzinfo=UTC)
    assert score_log_line(_line("error", "x"), anchor, [], set()) == 5
    assert score_log_line(_line("warn", "x"), anchor, [], set()) == 3
    assert score_log_line(_line("info", "x"), anchor, [], set()) == 1
    assert score_log_line(_line("debug", "x"), anchor, [], set()) == 0


def test_score_subject_token_boost_capped() -> None:
    anchor = datetime(2026, 5, 10, 12, 0, 0, tzinfo=UTC)
    line = _line("info", "timeout in network on station with network")
    # 'timeout', 'network', 'station' all in subject_tokens; cap at +6.
    score = score_log_line(line, anchor, ["timeout", "network", "station", "extra"], set())
    # info(+1) + 6 (cap) = 7; not 1 + 8.
    assert score == 7


def test_score_anchor_proximity() -> None:
    anchor = datetime(2026, 5, 10, 12, 0, 0, tzinfo=UTC)
    near = _line("info", "x", ts=anchor + timedelta(seconds=30))
    far = _line("info", "x", ts=anchor + timedelta(minutes=10))
    assert score_log_line(near, anchor, [], set()) == 3  # info + proximity
    assert score_log_line(far, anchor, [], set()) == 1  # info only


def test_score_dedupe_penalty() -> None:
    anchor = datetime(2026, 5, 10, 12, 0, 0, tzinfo=UTC)
    line = _line("error", "duplicate")
    assert score_log_line(line, anchor, [], {"duplicate"}) == 2  # error(5) - dedupe(3)
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_context.py -v`
Expected: FAIL — function missing.

- [ ] **Step 3: Implement**

In `triage_cli/context.py`, add at the bottom:

```python
from datetime import datetime

from triage_cli.models import LogLine

_SEVERITY_SCORES = {"error": 5, "warn": 3, "info": 1, "debug": 0}


def score_log_line(
    line: LogLine,
    anchor: datetime | None,
    subject_tokens: list[str],
    already_kept_messages: set[str],
) -> int:
    """Score a log line by relevance for prompt inclusion."""
    score = _SEVERITY_SCORES.get(line.level.lower(), 0)

    msg_lower = line.message.lower()
    matches = sum(1 for t in subject_tokens if t in msg_lower)
    score += min(matches * 2, 6)

    if anchor is not None:
        delta = abs((line.timestamp - anchor).total_seconds())
        if delta <= 60:
            score += 2

    if line.message in already_kept_messages:
        score -= 3

    return score
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_context.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add triage_cli/context.py tests/test_context.py
git commit -m "feat(context): score_log_line with severity/subject/proximity/dedupe"
```

---

### Task 2.4: `build_log_section` (greedy fill + chrono re-sort + tiny-input fast path)

**Files:**
- Modify: `triage_cli/context.py`
- Modify: `tests/test_context.py`

- [ ] **Step 1: Write failing tests**

Append to `tests/test_context.py`:

```python
from triage_cli.context import build_log_section


def test_build_log_section_tiny_input_fast_path() -> None:
    """≤25 lines and ≤2000 estimated tokens → return everything unchanged."""
    anchor = datetime(2026, 5, 10, 12, 0, 0, tzinfo=UTC)
    lines = [_line("info", f"msg {i}") for i in range(10)]
    kept, summary = build_log_section(lines, anchor, "subject", budget=6000)
    assert summary.kept == 10
    assert summary.candidates == 10
    assert kept == lines  # untouched


def test_build_log_section_orders_kept_chronologically() -> None:
    """Selection is by score; output is by timestamp."""
    anchor = datetime(2026, 5, 10, 12, 0, 0, tzinfo=UTC)
    # Force scoring path: many low-relevance lines + a few high-relevance.
    lines = [
        _line("debug", f"noise {i}", ts=anchor + timedelta(seconds=i))
        for i in range(50)
    ]
    lines.append(_line("error", "important early", ts=anchor - timedelta(minutes=5)))
    lines.append(_line("error", "important late", ts=anchor + timedelta(minutes=5)))

    kept, summary = build_log_section(lines, anchor, "subject", budget=200)
    # The two errors should be selected first; chronological output puts early before late.
    msgs = [k.message for k in kept]
    assert msgs.index("important early") < msgs.index("important late")


def test_build_log_section_respects_token_budget() -> None:
    anchor = datetime(2026, 5, 10, 12, 0, 0, tzinfo=UTC)
    # 30 lines of error, each ~80 chars → 30 * 20 ≈ 600 estimated tokens
    lines = [_line("error", "x" * 80, ts=anchor) for _ in range(30)]
    kept, summary = build_log_section(lines, anchor, "subject", budget=200)
    assert summary.used_tokens <= 200
    assert summary.kept < summary.candidates
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_context.py -v`
Expected: FAIL.

- [ ] **Step 3: Implement**

In `triage_cli/context.py`, add at the bottom:

```python
def _render_line(line: LogLine) -> str:
    """Same shape used by TriageBundle.as_user_message — keep in sync."""
    return f"[{line.timestamp.isoformat()}] [{line.level.upper()}] {line.message}"


def build_log_section(
    lines: list[LogLine],
    anchor: datetime | None,
    subject: str,
    budget: int = 6000,
) -> tuple[list[LogLine], ContextSummary]:
    """Score, select, and chronologically order log lines within ``budget`` tokens.

    Returns ``(kept_lines, summary)`` — the caller renders. Tiny inputs
    (≤25 lines and ≤2000 estimated tokens) bypass scoring entirely.
    """
    candidates = len(lines)

    # Tiny-input fast path
    if candidates <= 25:
        rendered = "\n".join(_render_line(line) for line in lines)
        rendered_tokens = estimate_tokens(rendered)
        if rendered_tokens <= 2000:
            return lines, ContextSummary(
                candidates=candidates,
                kept=candidates,
                budget_tokens=budget,
                used_tokens=rendered_tokens,
            )

    subject_tokens = extract_subject_tokens(subject)
    already_kept_messages: set[str] = set()
    scored: list[tuple[int, datetime, int, LogLine]] = []
    for i, line in enumerate(lines):
        s = score_log_line(line, anchor, subject_tokens, already_kept_messages)
        scored.append((s, line.timestamp, i, line))

    # Sort: score desc, timestamp asc, original index asc
    scored.sort(key=lambda t: (-t[0], t[1], t[2]))

    kept: list[LogLine] = []
    used_tokens = 0
    for _, _, _, line in scored:
        rendered = _render_line(line)
        line_tokens = estimate_tokens(rendered) + 1  # +1 for the joining newline
        if used_tokens + line_tokens > budget:
            continue
        kept.append(line)
        used_tokens += line_tokens
        already_kept_messages.add(line.message)

    kept.sort(key=lambda line: line.timestamp)
    return kept, ContextSummary(
        candidates=candidates,
        kept=len(kept),
        budget_tokens=budget,
        used_tokens=used_tokens,
    )
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_context.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add triage_cli/context.py tests/test_context.py
git commit -m "feat(context): build_log_section with greedy budget fill"
```

---

### Task 2.5: Wire context builder into pipeline; attach `context_summary` to `TriageReport`

**Files:**
- Modify: `triage_cli/models.py`
- Modify: `triage_cli/pipeline.py`
- Modify: `tests/test_pipeline.py`
- Modify: `tests/test_models.py`

- [ ] **Step 1: Add a failing test that asserts kept lines and summary appear on the report**

Append to `tests/test_pipeline.py` a test that constructs a `triage_one` invocation with > 25 mocked log lines and asserts:
- `report.log_event_count` reflects the **original** count (kept this for backwards compat with the existing render)
- `report.context_summary.candidates` == original count
- `report.context_summary.kept < candidates`
- `bundle.log_lines` (passed into `_llm_triage`) is the **trimmed** list

(Use the existing pipeline-test patterns; mock `dd_client.get_logs` to return synthetic `LogLine` objects.)

- [ ] **Step 2: Run test to verify it fails**

Run: `pytest tests/test_pipeline.py -v -k context_summary`
Expected: FAIL.

- [ ] **Step 3: Add `context_summary` to `TriageReport` model**

In `triage_cli/models.py`:

```python
from triage_cli.context import ContextSummary  # noqa: E402  (or under TYPE_CHECKING)
```

Add to `TriageReport`:

```python
context_summary: ContextSummary | None = None
```

- [ ] **Step 4: Wire `build_log_section` into `pipeline.triage_one`**

In `triage_cli/pipeline.py`, after the Datadog query and before assembling the bundle:

```python
from triage_cli.context import build_log_section
...
# After log_lines, log_truncated = dd_client.get_logs(...)
context_summary = None
if log_lines:
    log_lines, context_summary = build_log_section(
        log_lines, anchor_dt, ticket.subject,
    )
    _vecho(
        verbose,
        f"context: {context_summary.candidates} candidates, "
        f"kept {context_summary.kept} ({context_summary.used_tokens} of "
        f"{context_summary.budget_tokens}-token budget)",
    )
```

Then in the final `return TriageReport(...)`:

```python
return TriageReport(
    **llm_out.model_dump(),
    ticket_id=ticket.id,
    site_name=site_entry.site_name,
    window=TimeWindow(start=start, end=end),
    sources=sources,
    log_event_count=context_summary.candidates if context_summary else len(log_lines),
    generated_at=datetime.now(UTC),
    redaction_summary=redaction_counts,
    context_summary=context_summary,  # <-- new
)
```

(Note: `log_event_count` keeps its existing meaning — total candidates seen — by reading from the summary.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `pytest -v`
Expected: full suite passes.

- [ ] **Step 6: Commit**

```bash
git add triage_cli/models.py triage_cli/pipeline.py tests/test_pipeline.py tests/test_models.py
git commit -m "feat(pipeline): integrate token-aware context builder; attach context_summary"
```

---

### Task 2.6: Render the elision footer

**Files:**
- Modify: `triage_cli/render.py`
- Modify: `tests/test_render.py`

- [ ] **Step 1: Add a failing test**

Append to `tests/test_render.py`:

```python
def test_render_includes_elision_note_when_lines_dropped(capsys) -> None:
    """If context_summary.kept < candidates, render appends a small footnote."""
    from triage_cli.context import ContextSummary
    from triage_cli.models import TriageReport, TimeWindow
    from triage_cli.render import print_note
    from datetime import datetime, UTC

    report = TriageReport(
        finding="x", confidence="medium", evidence=[], suggested_note="x",
        ticket_id=1, site_name="acme",
        window=TimeWindow(start=datetime.now(UTC), end=datetime.now(UTC)),
        sources=["zendesk"], log_event_count=200,
        generated_at=datetime.now(UTC),
        context_summary=ContextSummary(
            candidates=200, kept=47, budget_tokens=6000, used_tokens=5800,
        ),
    )

    print_note(report)
    out = capsys.readouterr().out
    assert "153 of 200 log lines elided" in out
```

(Adjust `print_note` to whatever the actual public function in `render.py` is — confirm by reading the module.)

- [ ] **Step 2: Run test to verify it fails**

Run: `pytest tests/test_render.py -v -k elision`
Expected: FAIL.

- [ ] **Step 3: Add the footnote to the renderer**

In `triage_cli/render.py`, locate the Rich layout assembly. After the existing evidence/log section, add:

```python
if (
    report.context_summary is not None
    and report.context_summary.kept < report.context_summary.candidates
):
    elided = report.context_summary.candidates - report.context_summary.kept
    total = report.context_summary.candidates
    # Plain text in both Rich and raw output paths.
    print(
        f"\nNote: {elided} of {total} log lines elided by relevance scoring "
        "(severity, subject match, anchor proximity).",
    )
```

(Match this print to the existing TTY-aware pattern in the file — if Rich is in use, use a `Console.print(...)` with a dim style. If raw stdout, plain `print` is fine.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_render.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add triage_cli/render.py tests/test_render.py
git commit -m "feat(render): elision footnote when context selection drops lines"
```

---

## Phase 3 — Inbox Tokens + Density Modes

### Task 3.1: Extract inline `DEFAULT_CSS` into `inbox.tcss` (visual parity)

**Files:**
- Create: `triage_cli/inbox/inbox.tcss`
- Modify: `triage_cli/inbox/app.py`
- Modify: `triage_cli/inbox/widgets.py`

- [ ] **Step 1: Read current inline CSS**

Open `triage_cli/inbox/app.py` (the `SiteInputModal.DEFAULT_CSS` block at line ~35 and any others) and `triage_cli/inbox/widgets.py` (the `ReportPaneWidget.DEFAULT_CSS` block at line ~134 and any others). Note the rules verbatim.

- [ ] **Step 2: Create `triage_cli/inbox/inbox.tcss` with all the existing rules**

Copy each `DEFAULT_CSS` block into `inbox.tcss`, prefixing each block with the widget's class name as the selector. For example:

```css
/* triage_cli/inbox/inbox.tcss */

SiteInputModal {
    align: center middle;
}
SiteInputModal Vertical {
    background: $surface;
    border: thick $primary;
    padding: 1 2;
    width: 70;
    height: auto;
}
SiteInputModal Label { margin-bottom: 1; }
SiteInputModal Input { margin-bottom: 1; }
SiteInputModal #buttons { layout: horizontal; height: auto; }
SiteInputModal Button { margin-right: 1; }

/* ReportPaneWidget — copy from widgets.py:134 verbatim */
ReportPaneWidget {
    /* ... existing rules ... */
}
```

- [ ] **Step 3: Point `InboxApp` at the CSS file and remove inline `DEFAULT_CSS` blocks**

In `triage_cli/inbox/app.py`:

```python
class InboxApp(App):
    CSS_PATH = "inbox.tcss"  # relative to the inbox/ directory
    BINDINGS = [...]
```

Delete the `DEFAULT_CSS = """..."""` blocks from `SiteInputModal` and any other widgets in this file.

In `triage_cli/inbox/widgets.py`, delete the `DEFAULT_CSS = """..."""` blocks from `ReportPaneWidget` and any other widgets.

- [ ] **Step 4: Manually verify visual parity**

Textual snapshot tests are nontrivial; this step is a manual smoke check. Run:

```
triage-cli inbox --view <known-view-id>
```

against your usual test view. Confirm the inbox still looks the same — no missing borders, no color changes, no layout breakage. If Rich/Textual reports a CSS parse error, fix the rule and retry.

If you don't have a live view available right now, at minimum run `pytest tests/test_inbox_app.py -v` to confirm the existing app-level tests still pass.

- [ ] **Step 5: Commit**

```bash
git add triage_cli/inbox/inbox.tcss triage_cli/inbox/app.py triage_cli/inbox/widgets.py
git commit -m "refactor(inbox): extract inline DEFAULT_CSS into inbox.tcss"
```

---

### Task 3.2: Replace literal colors with named tokens

**Files:**
- Modify: `triage_cli/inbox/inbox.tcss`

- [ ] **Step 1: Define the token surface at the top of `inbox.tcss`**

```css
/* triage_cli/inbox/inbox.tcss
 *
 * Organizational tokens — NOT a theme system. The palette doesn't change;
 * it's just named. See spec 2026-05-10-final-phase-design.md, Feature 3,
 * "Why 'tokens, not themes'". Adding a --theme flag is an explicit non-goal.
 */

$status-triaged:  $success;
$status-triaging: $warning;
$status-pending:  $secondary;
$status-failed:   $error;

$priority-urgent: $error;
$priority-high:   $warning;
$priority-normal: $foreground;
$priority-low:    $secondary;

$row-pad-compact:     0;
$row-pad-comfortable: 1;

$panel-border: $primary-darken-2;
```

- [ ] **Step 2: Replace literal colors in the existing rules below the token block**

Walk through the rules copied in Task 3.1; wherever a hard-coded color appears (e.g., `border: thick $primary` is *already* a Textual token, leave it; but `color: green` should become `color: $status-triaged`). The goal is *parity* — the rendered colors should be identical to what the inline CSS produced. If a rule uses a Textual built-in token (e.g., `$primary`, `$surface`), leave it as-is.

- [ ] **Step 3: Manual visual check again**

Same procedure as Task 3.1, Step 4. The inbox should look identical.

- [ ] **Step 4: Commit**

```bash
git add triage_cli/inbox/inbox.tcss
git commit -m "refactor(inbox): name semantic colors via tokens (not a theme system)"
```

---

### Task 3.3: `WatcherUIState` model + state schema migration v1 → v2

**Files:**
- Modify: `triage_cli/models.py`
- Modify: `triage_cli/watcher.py`
- Modify: `tests/test_watcher.py` (or `tests/test_inbox_state.py` — pick whichever currently owns state-shape tests)

- [ ] **Step 1: Add a failing test for the migration**

Append to `tests/test_watcher.py`:

```python
def test_state_migrates_v1_to_v2_on_read(tmp_path) -> None:
    """A v1 state file should be readable; the read should populate ui.density."""
    import json
    from triage_cli.watcher import State, load_state

    v1_file = tmp_path / "watcher-state-test.json"
    v1_file.write_text(json.dumps({
        "version": 1,
        "triaged": {"123": "2026-05-09T12:00:00+00:00"},
    }))

    state = load_state(v1_file)
    assert state.version == 2
    assert state.ui is not None
    assert state.ui.density == "comfortable"
    assert state.triaged == {"123": "2026-05-09T12:00:00+00:00"}


def test_state_density_round_trip(tmp_path) -> None:
    """Writing then reading a v2 state file preserves ui.density."""
    import json
    from triage_cli.watcher import State, WatcherUIState, save_state, load_state

    v2_state = State(
        version=2,
        triaged={"5": "2026-05-09T12:00:00+00:00"},
        ui=WatcherUIState(density="compact"),
    )
    target = tmp_path / "watcher-state-test.json"
    save_state(target, v2_state)

    loaded = load_state(target)
    assert loaded.ui is not None
    assert loaded.ui.density == "compact"
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_watcher.py -v -k "state_migrates or density_round_trip"`
Expected: FAIL.

- [ ] **Step 3: Add `WatcherUIState` to `models.py`**

```python
from typing import Literal

class WatcherUIState(BaseModel):
    """Per-view UI preferences persisted in the watcher state file."""
    density: Literal["compact", "comfortable"] = "comfortable"
```

- [ ] **Step 4: Update `triage_cli/watcher.py`**

Find `STATE_VERSION` (currently `1`). Bump to `2`. Find the `State` model and add:

```python
ui: WatcherUIState | None = None
```

Replace the existing version-mismatch raise with a forward migrator. Look for the load helper (likely `load_state(path)`); the failing branch currently raises on version != STATE_VERSION. Change it to:

```python
def load_state(path: Path) -> State:
    if not path.exists():
        return State(version=STATE_VERSION, triaged={}, ui=WatcherUIState())
    raw = json.loads(path.read_text())
    version = raw.get("version", 1)
    if version == STATE_VERSION:
        return State(**raw)
    if version == 1:
        # Forward-migrate: v1 had no ui block.
        return State(
            version=STATE_VERSION,
            triaged=raw.get("triaged", {}),
            ui=WatcherUIState(),  # default density
        )
    raise RuntimeError(f"Unknown watcher state version: {version}")
```

(`save_state` should already serialize all fields via `model_dump_json` or similar — confirm it includes the new `ui` block. `watch` callers that don't care about `ui` simply ignore it.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `pytest -v`
Expected: full suite passes.

- [ ] **Step 6: Commit**

```bash
git add triage_cli/models.py triage_cli/watcher.py tests/test_watcher.py
git commit -m "feat(watcher): state v2 with WatcherUIState; v1 forward-migrator"
```

---

### Task 3.4: Density-aware row layouts in `TicketListWidget`

**Files:**
- Modify: `triage_cli/inbox/widgets.py`
- Modify: `tests/test_inbox_app.py` (or whichever test file covers widget rendering)

- [ ] **Step 1: Add a failing test that constructs the widget in both densities**

Append to `tests/test_inbox_app.py` — follow the existing `async def run() / async with app.run_test() as pilot:` pattern in this file:

```python
def test_ticket_list_widget_density_compact(tmp_path: Path) -> None:
    """Compact density: TicketListWidget reports row_height == 1."""
    async def run() -> None:
        app = _build_app_with_one_row(tmp_path, density="compact")
        async with app.run_test():
            table = app.query_one("#list", TicketListWidget)
            assert table.density == "compact"
            # DataTable.row_height defaults to 1; comfortable should bump it.
            assert table.row_height == 1
    asyncio.run(run())


def test_ticket_list_widget_density_comfortable(tmp_path: Path) -> None:
    """Comfortable density: row_height == 2 to fit subject + requester line."""
    async def run() -> None:
        app = _build_app_with_one_row(tmp_path, density="comfortable")
        async with app.run_test():
            table = app.query_one("#list", TicketListWidget)
            assert table.density == "comfortable"
            assert table.row_height == 2
    asyncio.run(run())
```

You'll also add a small helper `_build_app_with_one_row(tmp_path, *, density)` next to the existing test helpers — it should mirror whatever the existing tests use to construct an `InboxApp` with a hydrated `RowEntry`, but with a `density=` argument plumbed through. If the existing tests use a fixture for app construction, extend it to accept `density`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `pytest tests/test_inbox_app.py -v -k density`
Expected: FAIL — `density` is not a recognized kwarg.

- [ ] **Step 3: Add `density` to `TicketListWidget` and switch row templates**

In `triage_cli/inbox/widgets.py`, update the `TicketListWidget` constructor:

```python
class TicketListWidget(DataTable):
    def __init__(self, *, density: str = "comfortable", **kwargs) -> None:
        super().__init__(**kwargs)
        self.density = density
```

Refactor the row-render helper (find the method that turns a `RowEntry` into the row tuple) so it switches:

```python
def _row_for(self, entry: RowEntry) -> tuple[str, ...]:
    if self.density == "compact":
        return self._compact_row(entry)
    return self._comfortable_row(entry)

def _compact_row(self, entry: RowEntry) -> tuple[str, ...]:
    # One-line row: status icon, ticket id, truncated subject, requester, updated_at
    ...

def _comfortable_row(self, entry: RowEntry) -> tuple[str, ...]:
    # Two-line row: line 1 = status + id + subject; line 2 = requester · updated_at
    # Textual DataTable supports newlines in cell content via height_style
    ...
```

(The exact renderer follows whatever the existing single-row code does today — copy and adapt.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_inbox_app.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add triage_cli/inbox/widgets.py tests/test_inbox_app.py
git commit -m "feat(inbox): density-aware row layouts in TicketListWidget"
```

---

### Task 3.5: `'d'` keybinding + `action_cycle_density`

**Files:**
- Modify: `triage_cli/inbox/app.py`
- Modify: `tests/test_inbox_app.py`

- [ ] **Step 1: Add a failing test**

Append to `tests/test_inbox_app.py` — follow the existing `async def run() / pilot.press(...)` pattern:

```python
def test_pressing_d_toggles_density_and_persists(tmp_path: Path) -> None:
    """The 'd' keybinding cycles density and writes the new value to state."""
    state_file = tmp_path / "watcher-state-test.json"

    async def run() -> None:
        app = _build_app_with_one_row(tmp_path, density="comfortable")
        # Force the app to use our state_file path — adjust to whatever the
        # existing inbox tests do to point at a tmp state path.
        app._state_path = state_file
        async with app.run_test() as pilot:
            await pilot.press("d")
            await pilot.pause()  # let the action settle
            table = app.query_one("#list", TicketListWidget)
            assert table.density == "compact"

        # State was persisted to disk
        import json
        loaded = json.loads(state_file.read_text())
        assert loaded["ui"]["density"] == "compact"

    asyncio.run(run())


def test_pressing_d_again_cycles_back(tmp_path: Path) -> None:
    """Two presses of 'd' return to the starting density."""
    async def run() -> None:
        app = _build_app_with_one_row(tmp_path, density="comfortable")
        async with app.run_test() as pilot:
            await pilot.press("d")
            await pilot.pause()
            await pilot.press("d")
            await pilot.pause()
            table = app.query_one("#list", TicketListWidget)
            assert table.density == "comfortable"
    asyncio.run(run())
```

- [ ] **Step 2: Run test to verify it fails**

Run: `pytest tests/test_inbox_app.py -v -k density`
Expected: FAIL.

- [ ] **Step 3: Add the binding and action**

In `triage_cli/inbox/app.py`:

1. In `BINDINGS`, add:

```python
Binding("d", "cycle_density", "density"),
```

2. On `InboxApp`, add:

```python
def action_cycle_density(self) -> None:
    current = self._state.ui.density if self._state.ui else "comfortable"
    new = "compact" if current == "comfortable" else "comfortable"
    if self._state.ui is None:
        self._state.ui = WatcherUIState(density=new)
    else:
        self._state.ui.density = new
    # Persist to state file
    save_state(self._state_path, self._state)
    # Re-render the list with the new density
    list_widget = self.query_one(TicketListWidget)
    list_widget.density = new
    list_widget.refresh()  # or rebuild rows; follow existing refresh pattern
    self.notify(f"density: {new}")
```

(Imports: `from triage_cli.models import WatcherUIState`; `from triage_cli.watcher import save_state` — confirm names against the actual module.)

3. On startup (in `__init__` or `on_mount`), read `self._state.ui.density` and pass it to `TicketListWidget(density=...)` when composing.

- [ ] **Step 4: Run tests to verify they pass**

Run: `pytest tests/test_inbox_app.py -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add triage_cli/inbox/app.py tests/test_inbox_app.py
git commit -m "feat(inbox): 'd' keybinding cycles density; persists in state"
```

---

### Task 3.6: Documentation for Feature 3

**Files:**
- Modify: `README.md`
- Modify: `docs/runbooks/07-inbox-mode.md`
- Modify: `docs/CHEATSHEET.md`

- [ ] **Step 1: README — keybindings list**

Add `d` to the inbox keybindings list with the description: "cycle row density (comfortable ↔ compact)".

- [ ] **Step 2: 07-inbox-mode.md — keybinding table**

Add the row to the keybinding table; mention persistence.

- [ ] **Step 3: CHEATSHEET.md — keybinding reference**

Add `d` to the inbox keybinding reference.

- [ ] **Step 4: Commit**

```bash
git add README.md docs/runbooks/07-inbox-mode.md docs/CHEATSHEET.md
git commit -m "docs: density toggle in inbox keybindings reference"
```

---

## Final Pass

### Task F.1: Re-run the full suite + cert script

- [ ] **Step 1: Full pytest run**

Run: `pytest -v`
Expected: all tests pass, no regressions.

- [ ] **Step 2: Lint**

Run: `ruff check .`
Expected: clean.

- [ ] **Step 3: Re-certify the read-only Zendesk boundary**

Run: `python scripts/certify_readonly_my_queue.py`
Expected: passes (the cert script's contract is unchanged; the redactor is invisible to it).

- [ ] **Step 4: Manual smoke**

```
triage-cli triage <known-ticket-id> --verbose
```

Confirm in stderr: `redacted: N phones, N addresses, N coords` and `context: N candidates, kept N (...)` lines appear.

```
triage-cli triage <known-ticket-id> --no-redact --verbose
```

Confirm: `redaction: disabled` appears.

```
triage-cli inbox --view <known-view-id>
```

Press `d` — confirm density flips and persists across restarts.

- [ ] **Step 5: Open a PR**

```bash
git push -u origin <branch>
gh pr create --title "1.0: PII redactor + token-aware context + inbox density" --body "$(cat <<'EOF'
## Summary
Implements the three scope-locked features from `docs/superpowers/specs/2026-05-10-final-phase-design.md`:
- PII redactor at the LLM boundary (caller PII → typed placeholders)
- Token-aware log selection in `pipeline.triage_one`
- Inbox design tokens + `'d'` density toggle

## Test plan
- [x] `pytest -v` — full suite green
- [x] `ruff check .` — clean
- [x] `scripts/certify_readonly_my_queue.py` — passes
- [x] Manual smoke: redaction stderr line shows; `--no-redact` disables; context elision footer renders; `'d'` toggles + persists

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

## Spec coverage check (self-review against spec)

| Spec section | Covered by task |
|---|---|
| Feature 1 — Categories (caller PII only) | 1.1 (phones), 1.2 (addresses), 1.3 (coords) |
| Feature 1 — Action (typed placeholder, no numbering) | 1.1, 1.2, 1.3 |
| Feature 1 — Placement (`llm.py` boundary, all 3 functions) | 1.6 |
| Feature 1 — Default on; `--no-redact` flag | 1.7 |
| Feature 1 — Pass-through guard | 1.4 (and via `_is_pre_redacted` introduced in 1.1) |
| Feature 1 — Stderr audit; `redaction: disabled` always | 1.6 |
| Feature 1 — `redaction_summary` in saved JSON | 1.5 + 1.8 |
| Feature 1 — Module structure (`redact.py`, modified `llm.py`/`cli.py`/`models.py`) | 1.1, 1.5, 1.6, 1.7 |
| Feature 1 — Tests | 1.1–1.4 |
| Feature 1 — No cert-script change | F.1 (re-run as integrity check) |
| Feature 1 — Doc updates incl. CLAUDE.md refinement | 1.9 |
| Feature 2 — Apply to `pipeline.triage_one`; not `extract_anchor`/investigate | 2.5 |
| Feature 2 — Scoring weights (severity/subject/proximity/dedupe) | 2.3 |
| Feature 2 — Token budget (6000 logs, char/4 estimator) | 2.1, 2.4 |
| Feature 2 — Selection algorithm (greedy fill, chrono re-sort) | 2.4 |
| Feature 2 — Tiny-input fast path | 2.4 |
| Feature 2 — Visibility (verbose stderr line) | 2.5 |
| Feature 2 — Render elision footer | 2.6 |
| Feature 2 — `context_summary` on `TriageReport` | 2.5 |
| Feature 2 — Module structure (`context.py`, modified pipeline/render/models) | 2.1–2.6 |
| Feature 2 — Tests | 2.1–2.4, 2.5, 2.6 |
| Feature 3 — Token surface in inbox.tcss | 3.1, 3.2 |
| Feature 3 — `compact` / `comfortable` density toggle (`d` key) | 3.4, 3.5 |
| Feature 3 — Default `comfortable` | 3.3, 3.5 |
| Feature 3 — Persistence in watcher state | 3.3, 3.5 |
| Feature 3 — State schema v1 → v2 migration | 3.3 |
| Feature 3 — `WatcherUIState` model | 3.3 |
| Feature 3 — Notification on toggle | 3.5 |
| Feature 3 — Doc updates | 3.6 |
| Cross-cutting — full pytest + ruff + cert + smoke | F.1 |
| Cross-cutting — no new runtime deps | (enforced — no `pip install` in any task) |
| Cross-cutting — stdout/stderr discipline | (followed — every diagnostic uses stderr or `notify()`) |

All spec requirements have at least one task. No gaps identified.

## Open questions to revisit during implementation

These are the spec's Q1–Q7 that the user accepted as defaults but flagged for possible adjustment. If any of them surface during implementation (e.g., test cases force the issue), pause and ask before changing the default:

- Q1: phone regex precision (currently permissive)
- Q2: scoring weights (5/3/1/0; +2 subject, cap +6; +2 proximity; −3 dedupe)
- Q3: density default = `comfortable`
- Q4: stderr redaction line only on `--verbose` (always for `--no-redact`)
- Q5: `redaction_summary` in JSON only (not in saved markdown)
- Q6: forward-migrate state on first read
- Q7: address regex = street-line only (preserves city/state/ZIP for site extraction)
