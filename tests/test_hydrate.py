"""Tests for inbox.hydrate.recent_reports."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

from triage_cli.inbox import hydrate
from triage_cli.models import TimeWindow, TriageReport


def _write_report(notes_dir: Path, ticket_id: int, generated_at: datetime) -> None:
    notes_dir.mkdir(parents=True, exist_ok=True)
    stem = f"{ticket_id}-{generated_at.strftime('%Y%m%dT%H%M%SZ')}"
    report = TriageReport(
        finding="x",
        confidence="low",
        evidence=[],
        suggested_note="y",
        next_checks=[],
        unknowns=[],
        ticket_id=ticket_id,
        site_name="aurora-pd",
        window=TimeWindow(start=generated_at, end=generated_at),
        sources=["zendesk"],
        log_event_count=0,
        generated_at=generated_at,
    )
    (notes_dir / f"{stem}.json").write_text(report.model_dump_json(indent=2), encoding="utf-8")


def test_recent_reports_filters_by_age(tmp_path: Path) -> None:
    now = datetime.now(UTC)
    fresh = now - timedelta(hours=1)
    stale = now - timedelta(hours=48)
    _write_report(tmp_path, 1, fresh)
    _write_report(tmp_path, 2, stale)

    out = hydrate.recent_reports(tmp_path, hours=24)
    assert len(out) == 1
    assert out[0].ticket_id == 1


def test_recent_reports_dedupes_latest_per_ticket(tmp_path: Path) -> None:
    now = datetime.now(UTC)
    older = now - timedelta(hours=2)
    newer = now - timedelta(hours=1)
    _write_report(tmp_path, 42, older)
    _write_report(tmp_path, 42, newer)

    out = hydrate.recent_reports(tmp_path, hours=24)
    assert len(out) == 1
    assert out[0].generated_at == newer


def test_recent_reports_skips_corrupt_files(tmp_path: Path) -> None:
    (tmp_path / "999-bad.json").write_text("{not json", encoding="utf-8")
    fresh = datetime.now(UTC) - timedelta(hours=1)
    _write_report(tmp_path, 1, fresh)

    out = hydrate.recent_reports(tmp_path, hours=24)
    assert len(out) == 1
    assert out[0].ticket_id == 1


def test_recent_reports_missing_dir_returns_empty(tmp_path: Path) -> None:
    out = hydrate.recent_reports(tmp_path / "does-not-exist", hours=24)
    assert out == []


def test_recent_reports_sorted_newest_first(tmp_path: Path) -> None:
    now = datetime.now(UTC)
    _write_report(tmp_path, 1, now - timedelta(hours=3))
    _write_report(tmp_path, 2, now - timedelta(hours=1))
    _write_report(tmp_path, 3, now - timedelta(hours=2))

    out = hydrate.recent_reports(tmp_path, hours=24)
    ids = [report.ticket_id for report in out]
    assert ids == [2, 3, 1]
