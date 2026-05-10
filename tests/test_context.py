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


from triage_cli.context import extract_subject_tokens


def test_extract_subject_tokens_lowercases() -> None:
    assert extract_subject_tokens("SIP TIMEOUT outage") == ["timeout", "outage"]


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
