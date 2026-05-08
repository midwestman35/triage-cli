"""Read TriageReport JSON sidecars from disk for inbox startup hydration."""

from __future__ import annotations

import json
import logging
from datetime import UTC, datetime, timedelta
from pathlib import Path

from pydantic import ValidationError

from triage_cli.models import TriageReport

logger = logging.getLogger(__name__)


def recent_reports(notes_dir: Path, *, hours: int = 24) -> list[TriageReport]:
    """Return recent report sidecars, deduped by ticket and sorted newest first."""
    if not notes_dir.exists():
        return []

    cutoff = datetime.now(UTC) - timedelta(hours=hours)
    by_ticket: dict[int, TriageReport] = {}

    for json_path in notes_dir.glob("*.json"):
        try:
            report = TriageReport.model_validate_json(json_path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError, ValidationError) as exc:
            logger.warning("hydrate: skipping corrupt sidecar %s: %s", json_path.name, exc)
            continue

        if report.generated_at < cutoff:
            continue

        current = by_ticket.get(report.ticket_id)
        if current is None or report.generated_at > current.generated_at:
            by_ticket[report.ticket_id] = report

    return sorted(by_ticket.values(), key=lambda report: report.generated_at, reverse=True)
