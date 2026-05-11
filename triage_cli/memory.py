"""Durable investigation memory: MEMORY.md (source of truth) + SQLite FTS5 index.

Human workflow: edit MEMORY.md to prune entries; the FTS5 index rebuilds
automatically on the next call when MEMORY.md mtime > last_indexed_at.
"""
from __future__ import annotations

import sqlite3
from datetime import UTC, datetime
from pathlib import Path

from triage_cli.models import MemoryEntry

MEMORY_MD = Path("MEMORY.md")
MEMORY_DB = Path("data/memory.db")

_HEADER = (
    "# Investigation Memory\n\n"
    "<!-- Append-only by the tool. Delete entries to prune. "
    "Search index rebuilds automatically. -->\n"
)


def _ensure_db(db_path: Path) -> sqlite3.Connection:
    db_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(db_path)
    conn.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS investigations USING fts5("
        "ticket_id, customer, subject, symptom, assessment, resolution)"
    )
    conn.execute(
        "CREATE TABLE IF NOT EXISTS memory_meta "
        "(key TEXT PRIMARY KEY, value TEXT NOT NULL)"
    )
    conn.commit()
    return conn


def _needs_rebuild(conn: sqlite3.Connection) -> bool:
    if not MEMORY_MD.exists():
        return False
    row = conn.execute(
        "SELECT value FROM memory_meta WHERE key='last_indexed_at'"
    ).fetchone()
    if row is None:
        return True
    last_indexed = datetime.fromisoformat(row[0])
    mtime = datetime.fromtimestamp(MEMORY_MD.stat().st_mtime, tz=UTC)
    return mtime > last_indexed


def _parse_memory_md() -> list[dict[str, str]]:
    if not MEMORY_MD.exists():
        return []
    entries: list[dict[str, str]] = []
    for block in MEMORY_MD.read_text().split("---"):
        block = block.strip()
        if not block or block.startswith("#") or block.startswith("<!--"):
            continue
        entry: dict[str, str] = {}
        for line in block.splitlines():
            if ":" in line:
                key, _, value = line.partition(":")
                entry[key.strip()] = value.strip()
        if "id" in entry and "subject" in entry:
            entries.append(entry)
    return entries


def rebuild_index(db_path: Path = MEMORY_DB) -> int:
    """Parse MEMORY.md and rebuild the FTS5 index from scratch. Returns entry count."""
    conn = _ensure_db(db_path)
    entries = _parse_memory_md()
    conn.execute("DELETE FROM investigations")
    for e in entries:
        conn.execute(
            "INSERT INTO investigations VALUES (?,?,?,?,?,?)",
            (
                e.get("id", ""),
                e.get("customer", ""),
                e.get("subject", ""),
                e.get("symptom", ""),
                e.get("assessment", ""),
                e.get("resolution", "[unknown]"),
            ),
        )
    conn.execute(
        "INSERT OR REPLACE INTO memory_meta VALUES ('last_indexed_at', ?)",
        (datetime.now(UTC).isoformat(),),
    )
    conn.execute(
        "INSERT OR REPLACE INTO memory_meta VALUES ('entry_count', ?)",
        (str(len(entries)),),
    )
    conn.commit()
    conn.close()
    return len(entries)


def retrieve_similar(
    subject: str,
    symptom: str,
    *,
    limit: int = 3,
    db_path: Path = MEMORY_DB,
) -> list[MemoryEntry]:
    """FTS5 BM25 query against subject + first 500 chars of symptom."""
    conn = _ensure_db(db_path)
    if _needs_rebuild(conn):
        conn.close()
        rebuild_index(db_path)
        conn = _ensure_db(db_path)
    query = f"{subject} {symptom[:500]}"
    try:
        rows = conn.execute(
            "SELECT ticket_id, customer, subject, symptom, assessment, resolution "
            "FROM investigations WHERE investigations MATCH ? "
            "ORDER BY bm25(investigations) LIMIT ?",
            (query, limit),
        ).fetchall()
    except sqlite3.OperationalError:
        rows = []
    conn.close()
    return [
        MemoryEntry(
            ticket_id=r[0], customer=r[1], subject=r[2],
            symptom=r[3], assessment=r[4], resolution=r[5],
        )
        for r in rows
    ]


def find_duplicate(ticket_id: str, *, db_path: Path = MEMORY_DB) -> MemoryEntry | None:
    """Exact ticket_id lookup. Returns None when not found."""
    conn = _ensure_db(db_path)
    if _needs_rebuild(conn):
        conn.close()
        rebuild_index(db_path)
        conn = _ensure_db(db_path)
    row = conn.execute(
        "SELECT ticket_id, customer, subject, symptom, assessment, resolution "
        "FROM investigations WHERE ticket_id = ?",
        (ticket_id,),
    ).fetchone()
    conn.close()
    if row is None:
        return None
    return MemoryEntry(
        ticket_id=row[0], customer=row[1], subject=row[2],
        symptom=row[3], assessment=row[4], resolution=row[5],
    )


def append_investigation(
    ticket_id: str,
    customer: str,
    subject: str,
    symptom: str,
    assessment: str,
    resolution: str = "[unknown]",
    *,
    db_path: Path = MEMORY_DB,
) -> None:
    """Append one entry to MEMORY.md and insert into the FTS5 index."""
    _ensure_memory_md()
    entry_text = (
        f"\n---\n"
        f"id: {ticket_id}\n"
        f"customer: {customer}\n"
        f"date: {datetime.now(UTC).replace(microsecond=0).isoformat()}\n"
        f"subject: {subject}\n"
        f"symptom: {symptom[:500]}\n"
        f"assessment: {assessment}\n"
        f"resolution: {resolution}\n"
        f"---\n"
    )
    with MEMORY_MD.open("a") as f:
        f.write(entry_text)

    conn = _ensure_db(db_path)
    conn.execute(
        "INSERT INTO investigations VALUES (?,?,?,?,?,?)",
        (ticket_id, customer, subject, symptom[:500], assessment, resolution),
    )
    conn.execute(
        "INSERT OR REPLACE INTO memory_meta VALUES ('last_indexed_at', ?)",
        (datetime.now(UTC).isoformat(),),
    )
    conn.commit()
    conn.close()


def _ensure_memory_md() -> None:
    if not MEMORY_MD.exists():
        MEMORY_MD.write_text(_HEADER)
