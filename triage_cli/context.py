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


from datetime import datetime

from triage_cli.models import LogLine

_SEVERITY_SCORES = {"error": 5, "warn": 3, "info": 1, "debug": 0}


def score_log_line(
    line: LogLine,
    anchor: datetime | None,
    subject_tokens: list[str],
    already_kept_messages: set[str],
) -> int:
    """Score a log line by relevance for prompt inclusion."""
    score = _SEVERITY_SCORES.get(line.level.lower(), 0)

    msg_lower = line.message.lower()
    matches = sum(1 for t in subject_tokens if t in msg_lower)
    score += min(matches * 2, 6)

    if anchor is not None:
        delta = abs((line.timestamp - anchor).total_seconds())
        if delta <= 60:
            score += 2

    if line.message in already_kept_messages:
        score -= 3

    return score
