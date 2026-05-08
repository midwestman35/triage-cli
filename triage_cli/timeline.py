"""Timeline events: the canonical normalized shape across all evidence sources.

A `TimelineEvent` is one parsed line from a log, one Zendesk comment, or one
synthetic event (e.g. ticket created). Parsers in `evidence.py` and the
investigation orchestration in `investigation.py` produce these; the LLM
assessment prompt consumes the merged stream.

Two parsers are provided here:
- ISO-8601-prefix lines (the common log shape on this platform).
- JSON-line logs with a `timestamp` / `@timestamp` / `time` key.

Lines that match neither are returned in the unparsed counter; callers decide
whether to surface them as untimestamped events or drop them.
"""
from __future__ import annotations

import json
import re
from datetime import UTC, datetime
from typing import Any

from pydantic import BaseModel, Field


class TimelineEvent(BaseModel):
    """One normalized event in an investigation's timeline.

    `timestamp` is None for events we couldn't time-anchor (e.g. an unparsed
    line we still want to surface). `source` is the human label of the
    EvidenceSource it came from; `kind` is a short tag like "log" or
    "zendesk_comment" that callers use to group/filter.
    """

    timestamp: datetime | None = None
    source: str
    kind: str
    level: str | None = None
    message: str
    attributes: dict[str, Any] = Field(default_factory=dict)


_ISO_PREFIX = re.compile(
    r"""^
    \[?                                 # optional opening bracket
    (?P<ts>
        \d{4}-\d{2}-\d{2}               # YYYY-MM-DD
        [T\ ]                           # T or space
        \d{2}:\d{2}:\d{2}               # HH:MM:SS
        (?:[.,]\d+)?                    # optional fractional seconds
        (?:Z|[+-]\d{2}:?\d{2})?         # optional offset
    )
    \]?
    \s*
    (?P<rest>.*)$
    """,
    re.VERBOSE,
)

_LEVEL_RE = re.compile(
    r"^\[?(?P<level>ERROR|WARN(?:ING)?|INFO|DEBUG|TRACE|FATAL|CRITICAL)\]?\s*[:|\-]?\s*",
    re.IGNORECASE,
)


def _parse_timestamp(raw: str) -> datetime | None:
    """Best-effort parse of a timestamp string. Returns UTC-aware or None."""
    s = raw.replace(",", ".").replace(" ", "T", 1)
    if s.endswith("Z"):
        s = s[:-1] + "+00:00"
    try:
        dt = datetime.fromisoformat(s)
    except ValueError:
        return None
    return dt.replace(tzinfo=UTC) if dt.tzinfo is None else dt.astimezone(UTC)


def _parse_iso_line(line: str, source: str) -> TimelineEvent | None:
    """Try the ISO-8601-prefix pattern; return None if no timestamp at line start."""
    m = _ISO_PREFIX.match(line)
    if not m:
        return None
    dt = _parse_timestamp(m.group("ts"))
    if dt is None:
        return None
    rest = m.group("rest").strip()
    level: str | None = None
    lvl_match = _LEVEL_RE.match(rest)
    if lvl_match:
        level = lvl_match.group("level").upper()
        rest = rest[lvl_match.end():]
    return TimelineEvent(
        timestamp=dt, source=source, kind="log", level=level, message=rest,
    )


_JSON_TS_KEYS = ("timestamp", "@timestamp", "time", "ts")
_JSON_MSG_KEYS = ("message", "msg", "log")
_JSON_LEVEL_KEYS = ("level", "severity", "log_level")


def _parse_json_line(line: str, source: str) -> TimelineEvent | None:
    """Try parsing the line as JSON with a recognized timestamp key."""
    line = line.strip()
    if not line.startswith("{"):
        return None
    try:
        data = json.loads(line)
    except json.JSONDecodeError:
        return None
    if not isinstance(data, dict):
        return None
    ts: datetime | None = None
    for key in _JSON_TS_KEYS:
        val = data.get(key)
        if isinstance(val, str):
            ts = _parse_timestamp(val)
            if ts is not None:
                break
    if ts is None:
        return None
    msg = ""
    for key in _JSON_MSG_KEYS:
        val = data.get(key)
        if isinstance(val, str) and val:
            msg = val
            break
    level: str | None = None
    for key in _JSON_LEVEL_KEYS:
        val = data.get(key)
        if isinstance(val, str) and val:
            level = val.upper()
            break
    return TimelineEvent(
        timestamp=ts, source=source, kind="log", level=level, message=msg, attributes=data,
    )


def parse_lines(text: str, source: str) -> tuple[list[TimelineEvent], int]:
    """Parse raw text into TimelineEvents; return (events, unparsed_line_count).

    Tries ISO-8601 prefix first, then JSON-line. Lines matching neither are
    counted but not emitted as events — callers can surface the count via
    EvidenceSource.notes.
    """
    events: list[TimelineEvent] = []
    unparsed = 0
    for raw in text.splitlines():
        line = raw.rstrip()
        if not line:
            continue
        ev = _parse_iso_line(line, source) or _parse_json_line(line, source)
        if ev is None:
            unparsed += 1
            continue
        events.append(ev)
    return events, unparsed


def merge(*streams: list[TimelineEvent]) -> list[TimelineEvent]:
    """Merge timeline streams into a single chronological list.

    Events with timestamp=None sort to the end (stable, by insertion order).
    """
    flat: list[TimelineEvent] = []
    for s in streams:
        flat.extend(s)
    flat.sort(key=lambda e: (e.timestamp is None, e.timestamp or datetime.min.replace(tzinfo=UTC)))
    return flat
