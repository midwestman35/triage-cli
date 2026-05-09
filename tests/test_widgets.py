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
