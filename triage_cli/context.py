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
