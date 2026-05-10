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
