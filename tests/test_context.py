# tests/test_context.py
"""Tests for triage_cli.context (token-aware log selection)."""
from datetime import UTC, datetime, timedelta

from triage_cli.context import (
    ContextSummary,
    build_log_section,
    estimate_tokens,
    extract_subject_tokens,
    score_log_line,
)
from triage_cli.models import LogLine


def test_estimate_tokens_is_chars_over_four() -> None:
    assert estimate_tokens("") == 0
    assert estimate_tokens("a" * 4) == 1
    assert estimate_tokens("a" * 100) == 25


def test_context_summary_fields() -> None:
    s = ContextSummary(candidates=200, kept=47, budget_tokens=6000, used_tokens=5921)
    assert s.candidates == 200
    assert s.kept == 47


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
