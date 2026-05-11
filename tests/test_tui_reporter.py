"""Tests for TUIReporter — the asyncio-queue-based reporter for the TUI."""
from __future__ import annotations

import asyncio

import pytest


def test_tui_reporter_phase_started_puts_event_on_queue():
    from triage_cli.tui.reporter import PhaseStarted, TUIReporter

    async def run():
        queue: asyncio.Queue = asyncio.Queue()
        r = TUIReporter(queue)
        r.phase_started("customer_history", "fetching")
        event = queue.get_nowait()
        assert isinstance(event, PhaseStarted)
        assert event.phase == "customer_history"
        assert event.detail == "fetching"

    asyncio.run(run())


def test_tui_reporter_phase_done_puts_event_on_queue():
    from triage_cli.tui.reporter import PhaseDone, TUIReporter

    async def run():
        queue: asyncio.Queue = asyncio.Queue()
        r = TUIReporter(queue)
        r.phase_done("memory_lookup", "3 found")
        event = queue.get_nowait()
        assert isinstance(event, PhaseDone)
        assert event.phase == "memory_lookup"

    asyncio.run(run())


def test_tui_reporter_phase_failed_puts_event_on_queue():
    from triage_cli.tui.reporter import PhaseFailed, TUIReporter

    async def run():
        queue: asyncio.Queue = asyncio.Queue()
        r = TUIReporter(queue)
        r.phase_failed("llm_call", RuntimeError("timeout"))
        event = queue.get_nowait()
        assert isinstance(event, PhaseFailed)
        assert "timeout" in str(event.err)

    asyncio.run(run())


def test_tui_reporter_satisfies_reporter_protocol():
    from triage_cli.pipeline import Reporter
    from triage_cli.tui.reporter import TUIReporter

    queue: asyncio.Queue = asyncio.Queue()
    r = TUIReporter(queue)
    assert isinstance(r, Reporter)
