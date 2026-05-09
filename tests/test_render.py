"""Tests for the TriageReport renderer."""

from __future__ import annotations

import io
import json
from datetime import UTC, datetime
from pathlib import Path

from rich.console import Console

from triage_cli import render
from triage_cli.models import EvidenceItem, TimeWindow, TriageReport


def _report(**overrides) -> TriageReport:
    now = datetime(2026, 5, 7, 14, 0, 0, tzinfo=UTC)
    end = datetime(2026, 5, 7, 14, 15, 0, tzinfo=UTC)
    base = {
        "finding": "Station CH-22 may be failing SIP auth.",
        "confidence": "medium",
        "evidence": [
            EvidenceItem(timestamp=now, service="auth-service", message="401 Unauthorized"),
            EvidenceItem(timestamp=now, service="sip-edge", message="INVITE retry"),
        ],
        "suggested_note": "Reviewed Datadog logs for CH-22...",
        "next_checks": ["Verify station credentials"],
        "unknowns": ["Whether config changed"],
        "ticket_id": 12345,
        "site_name": "chicago-pd",
        "window": TimeWindow(start=now, end=end),
        "sources": ["zendesk", "datadog"],
        "log_event_count": 8,
        "generated_at": end,
    }
    base.update(overrides)
    return TriageReport(**base)


def test_to_markdown_includes_all_sections() -> None:
    md = render.to_markdown(_report())
    assert "# Triage Report — ZD-12345" in md
    assert "**Confidence:** medium" in md
    assert "## Finding" in md
    assert "## Evidence" in md
    assert "## Next Checks" in md
    assert "## Unknowns" in md
    assert "## Suggested Internal Note" in md


def test_to_markdown_omits_empty_optional_sections() -> None:
    md = render.to_markdown(_report(next_checks=[], unknowns=[]))
    assert "## Next Checks" not in md
    assert "## Unknowns" not in md
    assert "## Finding" in md
    assert "## Suggested Internal Note" in md


def test_to_markdown_is_deterministic() -> None:
    report = _report()
    assert render.to_markdown(report) == render.to_markdown(report)


def test_save_note_writes_md_and_json(tmp_path: Path) -> None:
    md_path, json_path = render.save_note(_report(), 12345, output_dir=tmp_path)
    assert md_path.exists()
    assert json_path.exists()
    assert md_path.suffix == ".md"
    assert json_path.suffix == ".json"
    payload = json.loads(json_path.read_text(encoding="utf-8"))
    assert payload["ticket_id"] == 12345
    assert payload["confidence"] == "medium"


def test_save_note_filenames_share_timestamp(tmp_path: Path) -> None:
    md_path, json_path = render.save_note(_report(), 12345, output_dir=tmp_path)
    assert md_path.stem == json_path.stem


def test_save_note_does_not_overwrite_same_second(tmp_path: Path, monkeypatch) -> None:
    class FixedDatetime(datetime):
        @classmethod
        def now(cls, tz=None):  # noqa: ANN001
            return datetime(2026, 5, 7, 14, 15, 0, tzinfo=tz)

    monkeypatch.setattr(render, "datetime", FixedDatetime)
    first_md, first_json = render.save_note(_report(), 12345, output_dir=tmp_path)
    second_md, second_json = render.save_note(_report(finding="new"), 12345, output_dir=tmp_path)

    assert first_md != second_md
    assert first_json != second_json
    assert first_md.read_text(encoding="utf-8") != second_md.read_text(encoding="utf-8")


def test_print_note_to_non_tty_emits_markdown() -> None:
    buf = io.StringIO()
    console = Console(file=buf, force_terminal=False, width=120)
    render.print_note(_report(), console=console)
    out = buf.getvalue()
    assert "# Triage Report — ZD-12345" in out
    assert "## Suggested Internal Note" in out


def test_print_note_to_tty_uses_rich() -> None:
    buf = io.StringIO()
    console = Console(file=buf, force_terminal=True, width=120, color_system=None)
    render.print_note(_report(), console=console)
    out = buf.getvalue()
    assert "Finding" in out
    assert "Suggested Internal Note" in out


def test_save_note_to_workspace_dir(tmp_path: Path) -> None:
    """When output_dir is the per-ticket workspace, files land there."""
    from triage_cli.render import save_note

    ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)
    report = TriageReport(
        finding="x", confidence="low",
        evidence=[EvidenceItem(message="seen")],
        suggested_note="y",
        next_checks=[], unknowns=[],
        ticket_id=44496, site_name="aur",
        window=TimeWindow(start=ts, end=ts),
        sources=["zendesk"], log_event_count=0,
        generated_at=ts,
    )

    workspace = tmp_path / "44496"
    workspace.mkdir(parents=True)
    md_path, json_path = save_note(report, 44496, output_dir=workspace)

    assert md_path.parent == workspace
    assert json_path.parent == workspace
    assert md_path.exists()
    assert json_path.exists()


def test_save_note_strips_content_url_from_json(tmp_path: Path) -> None:
    """No matter what's in the report, the saved JSON has no content_url field."""
    import json as _json  # noqa: PLC0415

    from triage_cli.render import save_note

    ts = datetime(2026, 5, 7, 12, 0, 0, tzinfo=UTC)

    # The report itself doesn't carry attachments today; this test pins
    # behavior so that any future schema additions don't accidentally
    # serialize a content_url.
    report = TriageReport(
        finding="x", confidence="low",
        evidence=[EvidenceItem(message="seen")],
        suggested_note="y",
        next_checks=[], unknowns=[],
        ticket_id=44496, site_name="aur",
        window=TimeWindow(start=ts, end=ts),
        sources=["zendesk"], log_event_count=0,
        generated_at=ts,
    )

    workspace = tmp_path / "44496"
    workspace.mkdir(parents=True)
    _, json_path = save_note(report, 44496, output_dir=workspace)
    parsed = _json.loads(json_path.read_text(encoding="utf-8"))

    # Walk the dict; assert no content_url key appears anywhere.
    def has_key(obj, key):
        if isinstance(obj, dict):
            if key in obj:
                return True
            return any(has_key(v, key) for v in obj.values())
        if isinstance(obj, list):
            return any(has_key(v, key) for v in obj)
        return False

    assert not has_key(parsed, "content_url")
