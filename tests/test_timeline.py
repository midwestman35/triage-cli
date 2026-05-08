"""Tests for triage_cli.timeline: TimelineEvent + ISO/JSON line parsing."""
from __future__ import annotations

from datetime import UTC, datetime

from triage_cli.timeline import TimelineEvent, merge, parse_lines


def test_parse_iso_prefix_with_level() -> None:
    text = "2026-05-07T12:34:56Z [ERROR] connection reset by peer"
    events, unparsed = parse_lines(text, source="test.log")
    assert unparsed == 0
    assert len(events) == 1
    e = events[0]
    assert e.timestamp == datetime(2026, 5, 7, 12, 34, 56, tzinfo=UTC)
    assert e.level == "ERROR"
    assert "connection reset by peer" in e.message


def test_parse_iso_prefix_with_space_separator_and_fractional() -> None:
    text = "2026-05-07 12:34:56,789 INFO startup complete"
    events, unparsed = parse_lines(text, source="t.log")
    assert unparsed == 0
    assert len(events) == 1
    assert events[0].timestamp is not None
    assert events[0].timestamp.year == 2026
    assert events[0].level == "INFO"


def test_parse_iso_prefix_with_brackets() -> None:
    text = "[2026-05-07T12:34:56+00:00] WARN slow query"
    events, _ = parse_lines(text, source="t.log")
    assert len(events) == 1
    assert events[0].level == "WARN"


def test_parse_json_line_with_at_timestamp() -> None:
    text = '{"@timestamp":"2026-05-07T12:34:56Z","level":"error","message":"bad thing"}'
    events, unparsed = parse_lines(text, source="t.json")
    assert unparsed == 0
    assert len(events) == 1
    e = events[0]
    assert e.timestamp == datetime(2026, 5, 7, 12, 34, 56, tzinfo=UTC)
    assert e.level == "ERROR"
    assert e.message == "bad thing"
    assert e.attributes["level"] == "error"


def test_parse_unparsed_lines_counted() -> None:
    text = "no timestamp here\nstill no timestamp\n2026-05-07T12:00:00Z parsed"
    events, unparsed = parse_lines(text, source="t.log")
    assert unparsed == 2
    assert len(events) == 1


def test_parse_blank_lines_ignored() -> None:
    text = "\n\n2026-05-07T12:00:00Z hi\n\n"
    events, unparsed = parse_lines(text, source="t.log")
    assert unparsed == 0
    assert len(events) == 1


def test_merge_sorts_by_timestamp() -> None:
    a = TimelineEvent(timestamp=datetime(2026, 5, 7, 12, tzinfo=UTC),
                      source="a", kind="log", message="first")
    b = TimelineEvent(timestamp=datetime(2026, 5, 7, 14, tzinfo=UTC),
                      source="b", kind="log", message="third")
    c = TimelineEvent(timestamp=datetime(2026, 5, 7, 13, tzinfo=UTC),
                      source="c", kind="log", message="second")
    merged = merge([b, c], [a])
    assert [e.message for e in merged] == ["first", "second", "third"]


def test_merge_pushes_untimed_to_end() -> None:
    timed = TimelineEvent(timestamp=datetime(2026, 5, 7, 12, tzinfo=UTC),
                          source="a", kind="log", message="timed")
    untimed = TimelineEvent(source="b", kind="note", message="untimed")
    merged = merge([untimed, timed])
    assert merged[0].message == "timed"
    assert merged[1].message == "untimed"


def test_iso_with_garbage_timestamp_is_unparsed() -> None:
    text = "9999-99-99T99:99:99Z something"
    events, unparsed = parse_lines(text, source="t.log")
    assert unparsed == 1
    assert events == []


def test_json_without_timestamp_is_unparsed() -> None:
    text = '{"level":"error","message":"hi"}'
    events, unparsed = parse_lines(text, source="t.json")
    assert unparsed == 1
    assert events == []
