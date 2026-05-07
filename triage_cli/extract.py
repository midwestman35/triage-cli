"""Pure-function helpers for the triage pipeline: ID parsing, site lookup, time-window construction, and anchor resolution.

All datetimes returned are timezone-aware UTC. Naive inputs are assumed UTC.
The only function that performs I/O is `load_site_map`, which reads the
on-disk site map JSON.
"""
from __future__ import annotations

import json
import re
from datetime import datetime, timedelta, timezone
from pathlib import Path

from pydantic import ValidationError

from .models import AnchorSource, SiteEntry, Ticket

_TICKET_URL_RE = re.compile(r"/(?:agent/)?tickets/(\d+)(?:[/?#].*)?$")
_RAW_ID_RE = re.compile(r"^\d+$")


def parse_ticket_id(value: str) -> int:
    """Parse a Zendesk ticket ID from a raw number, a /agent/tickets/<id> URL, or a /tickets/<id> URL.

    Raises ValueError on unrecognized input (empty string, non-numeric junk, or
    a URL with no numeric tail).
    """
    if not value or not value.strip():
        raise ValueError("Empty ticket id")
    s = value.strip()
    if _RAW_ID_RE.match(s):
        return int(s)
    m = _TICKET_URL_RE.search(s)
    if m:
        return int(m.group(1))
    raise ValueError(f"Could not parse ticket id from: {value!r}")


def load_site_map(path: Path) -> list[SiteEntry]:
    """Load and validate cnc-map.json.

    Raises FileNotFoundError if the file is missing, ValueError if its
    contents are not a valid list of SiteEntry records.
    """
    if not path.exists():
        raise FileNotFoundError(f"Site map not found: {path}")
    try:
        raw = json.loads(path.read_text())
    except json.JSONDecodeError as e:
        raise ValueError(f"Site map is not valid JSON: {e}") from e
    if not isinstance(raw, list):
        raise ValueError("Site map root must be a JSON array")
    try:
        return [SiteEntry(**row) for row in raw]
    except ValidationError as e:
        raise ValueError(f"Site map contains invalid entries: {e}") from e


def lookup_site(
    ticket: Ticket,
    sites: list[SiteEntry],
    cnc_override: str | None = None,
    site_override: str | None = None,
) -> tuple[SiteEntry | None, str]:
    """Resolve which SiteEntry the ticket is about.

    Priority:
    1. site_override (raw site_name string) -- if it matches an entry's site_name
       (case-insensitive), return that entry; otherwise return a synthetic
       SiteEntry(friendly_name="(manual)", site_name=site_override, cnc="").
    2. cnc_override -- exact CNC UUID match (case-insensitive). Raises
       ValueError if not found in the map.
    3. Exact friendly_name match (case-insensitive) against ticket.requester_org.
    4. Substring match of any site_name in ticket.subject + ticket.description.
    5. Substring match of any friendly_name in ticket.subject + ticket.description.

    Returns (entry, strategy) where strategy is one of:
        "site_flag", "cnc_flag", "org_match", "site_substring",
        "friendly_substring", "no_match".
    Returns (None, "no_match") when no match -- caller decides interactive
    prompt vs. abort.
    """
    if site_override is not None:
        target = site_override.lower()
        for entry in sites:
            if entry.site_name.lower() == target:
                return entry, "site_flag"
        synthetic = SiteEntry(
            friendly_name="(manual)", site_name=site_override, cnc=""
        )
        return synthetic, "site_flag"

    if cnc_override is not None:
        target = cnc_override.lower()
        for entry in sites:
            if entry.cnc.lower() == target:
                return entry, "cnc_flag"
        raise ValueError(f"CNC override {cnc_override} not found in site map")

    org = (ticket.requester_org or "").strip().lower()
    if org:
        for entry in sites:
            if entry.friendly_name.lower() == org:
                return entry, "org_match"

    haystack = f"{ticket.subject}\n{ticket.description}".lower()

    for entry in sites:
        sn = entry.site_name.lower()
        if sn and sn in haystack:
            return entry, "site_substring"

    for entry in sites:
        fn = entry.friendly_name.lower()
        if fn and fn in haystack:
            return entry, "friendly_substring"

    return None, "no_match"


def build_window(anchor: datetime, minutes: int) -> tuple[datetime, datetime]:
    """Return (start, end) = (anchor - minutes, anchor + minutes), both UTC-normalized.

    Naive datetimes are treated as UTC. Aware datetimes are converted to UTC.
    Raises ValueError if minutes <= 0 (zero-width or inverted windows are
    a programming error, not a runtime condition).
    """
    if minutes <= 0:
        raise ValueError(f"window minutes must be positive, got {minutes}")
    if anchor.tzinfo is None:
        anchor_utc = anchor.replace(tzinfo=timezone.utc)
    else:
        anchor_utc = anchor.astimezone(timezone.utc)
    delta = timedelta(minutes=minutes)
    return anchor_utc - delta, anchor_utc + delta


def _to_utc(dt: datetime) -> datetime:
    """Normalize a datetime to timezone-aware UTC. Naive inputs are assumed UTC."""
    if dt.tzinfo is None:
        return dt.replace(tzinfo=timezone.utc)
    return dt.astimezone(timezone.utc)


def resolve_anchor(
    ticket: Ticket,
    at_flag: datetime | None,
    extracted: datetime | None,
) -> tuple[datetime, AnchorSource]:
    """Pick the anchor timestamp and report which source won.

    Priority: at_flag -> extracted -> ticket.created_at.
    Returns a (datetime, AnchorSource) tuple; the datetime is always
    timezone-aware UTC.
    """
    if at_flag is not None:
        return _to_utc(at_flag), AnchorSource.FLAG
    if extracted is not None:
        return _to_utc(extracted), AnchorSource.EXTRACTED
    return _to_utc(ticket.created_at), AnchorSource.CREATED_AT
