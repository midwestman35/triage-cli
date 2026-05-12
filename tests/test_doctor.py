"""Tests for the `doctor` CLI subcommand."""
from __future__ import annotations

from typer.testing import CliRunner

# Import cli at collection time so load_dotenv() fires before any monkeypatching.
from triage_cli.cli import app

_ZD_ENV = {
    "ZENDESK_SUBDOMAIN": "acme",
    "ZENDESK_EMAIL": "ops@acme.com",
    "ZENDESK_API_TOKEN": "tok",
}
_DD_ENV = {"DD_API_KEY": "ddkey", "DD_APP_KEY": "ddapp"}
_LLM_ENV = {"LLM_PROVIDER": "unleash", "UNLEASH_API_KEY": "key"}


def test_doctor_fails_on_missing_zendesk_vars(monkeypatch):
    """doctor exits 1 when required Zendesk env vars are absent."""
    # Patch os.environ so doctor reads cleared vars.
    for var in ("ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN"):
        monkeypatch.delenv(var, raising=False)
    for var, val in {**_LLM_ENV, **_DD_ENV}.items():
        monkeypatch.setenv(var, val)

    runner = CliRunner()
    result = runner.invoke(app, ["doctor"])
    assert result.exit_code == 1


def test_doctor_warns_on_missing_datadog(monkeypatch, tmp_path):
    """doctor exits 0 but emits a ⚠ warning when Datadog vars are absent."""
    monkeypatch.chdir(tmp_path)
    for var, val in {**_ZD_ENV, **_LLM_ENV}.items():
        monkeypatch.setenv(var, val)
    monkeypatch.delenv("DD_API_KEY", raising=False)
    monkeypatch.delenv("DD_APP_KEY", raising=False)

    runner = CliRunner()
    result = runner.invoke(app, ["doctor"])
    assert result.exit_code == 0
    assert "⚠" in result.output


def test_doctor_exits_0_when_all_critical_pass(monkeypatch, tmp_path):
    """doctor exits 0 when all critical env vars are set."""
    monkeypatch.chdir(tmp_path)
    for var, val in {**_ZD_ENV, **_LLM_ENV, **_DD_ENV}.items():
        monkeypatch.setenv(var, val)

    runner = CliRunner()
    result = runner.invoke(app, ["doctor"])
    assert result.exit_code == 0
