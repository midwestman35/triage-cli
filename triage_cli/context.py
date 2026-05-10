# triage_cli/context.py
"""Token-aware log selection for the triage prompt.

Spec 2026-05-10-final-phase-design.md, Feature 2.

Selection is deterministic: each line is scored by severity, subject-token
match, anchor proximity, and a dedupe penalty. The top-N lines that fit
within ``budget`` tokens are kept; selection is then re-sorted chronologically
for the prompt.
"""
from __future__ import annotations

import re
from datetime import datetime

from pydantic import BaseModel

from triage_cli.models import LogLine


class ContextSummary(BaseModel):
    """Audit summary of the log-selection step (attached to TriageReport)."""

    candidates: int
    kept: int
    budget_tokens: int
    used_tokens: int


def estimate_tokens(text: str) -> int:
    """Approximate tokens as ``len(text) // 4`` (no tokenizer dep, by design)."""
    return len(text) // 4


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


def _render_line(line: LogLine) -> str:
    """Same shape used by TriageBundle.as_user_message — keep in sync."""
    return f"[{line.timestamp.isoformat()}] [{line.level.upper()}] {line.message}"


def build_log_section(
    lines: list[LogLine],
    anchor: datetime | None,
    subject: str,
    budget: int = 6000,
) -> tuple[list[LogLine], ContextSummary]:
    """Score, select, and chronologically order log lines within ``budget`` tokens.

    Returns ``(kept_lines, summary)`` — the caller renders. Tiny inputs
    (≤25 lines and ≤2000 estimated tokens) bypass scoring entirely.
    """
    candidates = len(lines)

    # Tiny-input fast path
    if candidates <= 25:
        rendered = "\n".join(_render_line(line) for line in lines)
        rendered_tokens = estimate_tokens(rendered)
        if rendered_tokens <= 2000:
            return lines, ContextSummary(
                candidates=candidates,
                kept=candidates,
                budget_tokens=budget,
                used_tokens=rendered_tokens,
            )

    subject_tokens = extract_subject_tokens(subject)
    already_kept_messages: set[str] = set()
    scored: list[tuple[int, datetime, int, LogLine]] = []
    for i, line in enumerate(lines):
        s = score_log_line(line, anchor, subject_tokens, already_kept_messages)
        scored.append((s, line.timestamp, i, line))

    # Sort: score desc, timestamp asc, original index asc
    scored.sort(key=lambda t: (-t[0], t[1], t[2]))

    kept: list[LogLine] = []
    used_tokens = 0
    for _, _, _, line in scored:
        rendered = _render_line(line)
        line_tokens = estimate_tokens(rendered) + 1  # +1 for the joining newline
        if used_tokens + line_tokens > budget:
            continue
        kept.append(line)
        used_tokens += line_tokens
        already_kept_messages.add(line.message)

    kept.sort(key=lambda line: line.timestamp)
    return kept, ContextSummary(
        candidates=candidates,
        kept=len(kept),
        budget_tokens=budget,
        used_tokens=used_tokens,
    )
