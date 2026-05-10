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
