"""TUIReporter: pushes Reporter events onto an asyncio queue for the Textual app."""
from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Any

from triage_cli.models import TriageReport


@dataclass
class PhaseStarted:
    phase: str
    detail: str = ""


@dataclass
class PhaseDone:
    phase: str
    detail: str = ""


@dataclass
class PhaseFailed:
    phase: str
    err: Exception


@dataclass
class EvidenceAdded:
    item: Any


@dataclass
class InvestigationDone:
    report: TriageReport


ReporterEvent = PhaseStarted | PhaseDone | PhaseFailed | EvidenceAdded | InvestigationDone


class TUIReporter:
    """Bridges the pipeline Reporter protocol to the Textual event queue."""

    def __init__(self, queue: asyncio.Queue[ReporterEvent]) -> None:
        self._q = queue

    def phase_started(self, phase: str, detail: str = "") -> None:
        self._q.put_nowait(PhaseStarted(phase=phase, detail=detail))

    def phase_done(self, phase: str, detail: str = "") -> None:
        self._q.put_nowait(PhaseDone(phase=phase, detail=detail))

    def phase_failed(self, phase: str, err: Exception) -> None:
        self._q.put_nowait(PhaseFailed(phase=phase, err=err))

    def evidence_added(self, item: Any) -> None:
        self._q.put_nowait(EvidenceAdded(item=item))

    def done(self, report: TriageReport) -> None:
        self._q.put_nowait(InvestigationDone(report=report))
