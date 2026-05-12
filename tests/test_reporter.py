"""Tests for the Reporter protocol implementations."""
from __future__ import annotations


def test_stderr_reporter_phase_started_prints_to_stderr(capsys):
    from triage_cli.pipeline import StderrReporter
    r = StderrReporter(verbose=True)
    r.phase_started("customer_history", "fetching")
    captured = capsys.readouterr()
    assert "customer_history" in captured.err
    assert "fetching" in captured.err


def test_stderr_reporter_phase_done_always_prints(capsys):
    from triage_cli.pipeline import StderrReporter
    r = StderrReporter(verbose=False)  # verbose=False still prints phase_done
    r.phase_done("memory_lookup", "3 prior found")
    captured = capsys.readouterr()
    assert "memory_lookup" in captured.err


def test_stderr_reporter_phase_failed_prints_error(capsys):
    from triage_cli.pipeline import StderrReporter
    r = StderrReporter(verbose=True)
    r.phase_failed("llm_call", RuntimeError("timeout"))
    captured = capsys.readouterr()
    assert "llm_call" in captured.err
    assert "timeout" in captured.err


def test_silent_reporter_emits_nothing(capsys):
    from triage_cli.pipeline import SilentReporter
    r = SilentReporter()
    r.phase_started("x")
    r.phase_done("x")
    r.phase_failed("x", RuntimeError("err"))
    r.evidence_added(None)
    captured = capsys.readouterr()
    assert captured.out == ""
    assert captured.err == ""


def test_stderr_reporter_verbose_false_suppresses_phase_started(capsys):
    from triage_cli.pipeline import StderrReporter
    r = StderrReporter(verbose=False)
    r.phase_started("build_timeline")
    captured = capsys.readouterr()
    assert captured.err == ""


def test_reporter_protocol_is_satisfied_by_stderr_reporter():
    from triage_cli.pipeline import Reporter, StderrReporter
    r = StderrReporter()
    assert isinstance(r, Reporter)


def test_reporter_protocol_is_satisfied_by_silent_reporter():
    from triage_cli.pipeline import Reporter, SilentReporter
    r = SilentReporter()
    assert isinstance(r, Reporter)
