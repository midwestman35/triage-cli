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
