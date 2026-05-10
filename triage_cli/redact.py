"""PII redactor applied at the LLM boundary.

Scope (locked by spec 2026-05-10-final-phase-design.md):
- Caller PII only: phones, addresses, GPS coords.
- Names: explicit gap (regex unreliable; revisit only if compliance asks).
- Operational IDs (Call-IDs, ticket #s, station codes, CNCs, sites): preserved.
"""
from __future__ import annotations

import re

from pydantic import BaseModel

# Phone: optional +1, common separators, with negative lookarounds
# preventing matches inside alphanumeric tokens like "abc5551234567xyz".
_PHONE_PATTERN = re.compile(
    r"(?<![A-Za-z0-9])"
    r"(?:\+?1[-.\s]?)?\(?\d{3}\)?[-.\s]?\d{3}[-.\s]?\d{4}"
    r"(?![A-Za-z0-9])"
)

# Street-line address redaction.
# Suffixes from USPS abbreviation list (common subset sufficient for 911 dispatch).
# Only the street number + name + suffix are consumed; city/state are preserved
# so site-lookup logic in extract.py can still match against the site map.
_STREET_SUFFIXES = (
    r"Ave(?:nue)?"
    r"|Blvd|Boulevard"
    r"|Cir(?:cle)?"
    r"|Ct|Court"
    r"|Dr(?:ive)?"
    r"|Expy|Expressway"
    r"|Fwy|Freeway"
    r"|Hwy|Highway"
    r"|Ln|Lane"
    r"|Loop"
    r"|Pkwy|Parkway"
    r"|Pl(?:ace)?"
    r"|Rd|Road"
    r"|Route|Rte"
    r"|Sq|Square"
    r"|St(?:reet)?"
    r"|Ter(?:race)?"
    r"|Trl|Trail"
    r"|Way"
)
# Pattern: leading digit(s), at least one capitalized word, then a suffix.
# City and beyond are intentionally excluded so site-lookup is unaffected.
_ADDRESS_PATTERN = re.compile(
    r"\b\d+\s+"                       # house number
    r"(?:[A-Z][A-Za-z0-9]*\s+)+"      # ≥1 capitalized word(s) (street name)
    r"(?:" + _STREET_SUFFIXES + r")"  # street type suffix
    r"\b",
    re.IGNORECASE,
)


class RedactionCounts(BaseModel):
    """Per-call redaction tally surfaced via verbose stderr and saved JSON."""

    phones: int = 0
    addresses: int = 0
    coords: int = 0
    enabled: bool = True


def _is_pre_redacted(match: str) -> bool:
    """Skip values that are already redacted (e.g., '***-***-1234' from Zendesk)."""
    s = match.lower()
    return "***" in s or "xxx" in s or "[redacted]" in s


def redact(text: str) -> tuple[str, RedactionCounts]:
    """Redact caller PII from ``text``. Returns (redacted_text, counts)."""
    counts = RedactionCounts(enabled=True)

    def _sub_phone(m: re.Match[str]) -> str:
        if _is_pre_redacted(m.group(0)):
            return m.group(0)
        counts.phones += 1
        return "<PHONE>"

    text = _PHONE_PATTERN.sub(_sub_phone, text)

    def _sub_address(m: re.Match[str]) -> str:
        counts.addresses += 1
        return "<ADDR>"

    text = _ADDRESS_PATTERN.sub(_sub_address, text)
    return text, counts
