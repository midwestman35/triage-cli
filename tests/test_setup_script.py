"""Focused contract tests for the interactive setup engine."""

from __future__ import annotations

import importlib
import json
import sys
from pathlib import Path

import pytest


def _load_script():
    sys.modules.pop("triage_cli.setup", None)
    return importlib.import_module("triage_cli.setup")


def test_env_example_parser_preserves_keys_and_defaults(tmp_path: Path) -> None:
    script = _load_script()
    env_example = tmp_path / ".env.example"
    env_example.write_text(
        "\n".join(
            [
                "# local-only comments are ignored",
                "ZENDESK_SUBDOMAIN=",
                "ZENDESK_EMAIL=",
                "DD_SITE=datadoghq.com",
                "",
                "LLM_PROVIDER=unleash",
                "UNLEASH_API_KEY=",
                "UNLEASH_BASE_URL=https://e-api.unleash.so",
                "UNLEASH_ASSISTANT_ID=",
                "UNLEASH_ACCOUNT=",
                "ANTHROPIC_MODEL=claude-sonnet-4-6",
            ]
        ),
        encoding="utf-8",
    )

    values = script.read_env_values(env_example)

    assert list(values) == [
        "ZENDESK_SUBDOMAIN",
        "ZENDESK_EMAIL",
        "DD_SITE",
        "LLM_PROVIDER",
        "UNLEASH_API_KEY",
        "UNLEASH_BASE_URL",
        "UNLEASH_ASSISTANT_ID",
        "UNLEASH_ACCOUNT",
        "ANTHROPIC_MODEL",
    ]
    assert values["ZENDESK_SUBDOMAIN"] == ""
    assert values["DD_SITE"] == "datadoghq.com"
    assert values["LLM_PROVIDER"] == "unleash"
    assert values["UNLEASH_BASE_URL"] == "https://e-api.unleash.so"
    assert values["ANTHROPIC_MODEL"] == "claude-sonnet-4-6"


def test_validate_config_value_normalizes_and_rejects_invalid_inputs() -> None:
    script = _load_script()

    assert script.validate_zendesk_subdomain(" https://acme-support/ ") == "acme-support"
    assert script.validate_zendesk_email(" analyst@example.com ") == "analyst@example.com"
    assert script.validate_required_secret(" token-value ") == "token-value"
    assert script.validate_zendesk_subdomain("http://acme-support/") == "acme-support"
    assert script.validate_llm_provider(" Unleash ") == "unleash"
    assert script.validate_llm_provider("") == "unleash"

    with pytest.raises(ValueError, match="spaces"):
        script.validate_zendesk_subdomain("acme support")
    with pytest.raises(ValueError, match="@"):
        script.validate_zendesk_email("analyst.example.com")
    with pytest.raises(ValueError, match="required"):
        script.validate_required_secret(" ")
    with pytest.raises(ValueError, match="unleash or claude"):
        script.validate_llm_provider("codex")


def test_prereqs_default_unleash_does_not_require_claude(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    script = _load_script()
    checked: list[str] = []

    def fake_which(command: str) -> str | None:
        checked.append(command)
        if command == "python3.11":
            return f"/usr/bin/{command}"
        return None

    monkeypatch.delenv("LLM_PROVIDER", raising=False)
    monkeypatch.setattr(script, "ENV_PATH", tmp_path / ".env")
    monkeypatch.setattr(script.shutil, "which", fake_which)
    monkeypatch.setattr(
        script,
        "run_capture",
        lambda command: script.CommandResult(returncode=0, output="ok"),
    )

    script.run_prereqs()

    captured = capsys.readouterr()
    assert checked == ["python3.11"]
    assert "Unleash does not require a local LLM CLI" in captured.out


def test_prereqs_claude_provider_requires_claude(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    script = _load_script()
    env_path = tmp_path / ".env"
    env_path.write_text("LLM_PROVIDER=claude\n", encoding="utf-8")
    checked: list[str] = []

    def fake_which(command: str) -> str | None:
        checked.append(command)
        if command == "python3.11":
            return f"/usr/bin/{command}"
        return None

    monkeypatch.delenv("LLM_PROVIDER", raising=False)
    monkeypatch.setattr(script, "ENV_PATH", env_path)
    monkeypatch.setattr(script.shutil, "which", fake_which)
    monkeypatch.setattr(
        script,
        "run_capture",
        lambda command: script.CommandResult(returncode=0, output="ok"),
    )

    with pytest.raises(script.SetupError, match="Claude is only required"):
        script.run_prereqs()

    assert checked == ["python3.11", "claude"]


def test_prompt_config_value_retries_only_failed_field(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    script = _load_script()
    answers = iter(["bad email", "analyst@example.com"])
    prompts: list[str] = []

    def fake_input(prompt: str) -> str:
        prompts.append(prompt)
        return next(answers)

    monkeypatch.setattr("builtins.input", fake_input)
    monkeypatch.setattr(
        script.getpass,
        "getpass",
        lambda prompt: pytest.fail(f"unexpected secret prompt: {prompt}"),
    )

    value = script.prompt_env_value("ZENDESK_EMAIL", "")

    captured = capsys.readouterr()
    assert value == "analyst@example.com"
    assert len(prompts) == 2
    assert all("ZENDESK_EMAIL" in prompt for prompt in prompts)
    assert "ZENDESK_EMAIL" in captured.out


def test_checkpoint_state_resumes_from_first_incomplete_phase(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    script = _load_script()
    state_path = tmp_path / ".setup-state.json"
    state_path.write_text(
        json.dumps(
            {
                "setup_version": script.SETUP_VERSION,
                "completed_phases": ["PREREQS", "ENVIRONMENT"],
            }
        ),
        encoding="utf-8",
    )
    monkeypatch.setattr(script, "STATE_PATH", state_path)

    state = script.load_state()

    assert script.first_incomplete_phase(state) == script.Phase.CONFIG

    script.mark_phase_complete(state, script.Phase.CONFIG)
    updated = json.loads(state_path.read_text(encoding="utf-8"))
    assert updated == {
        "setup_version": script.SETUP_VERSION,
        "completed_phases": ["PREREQS", "ENVIRONMENT", "CONFIG"],
    }
    assert script.first_incomplete_phase(updated) == script.Phase.VERIFY


def test_config_phase_skips_existing_env_without_overwriting(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    script = _load_script()
    env_example = tmp_path / ".env.example"
    env_path = tmp_path / ".env"
    env_example.write_text("ZENDESK_SUBDOMAIN=\n", encoding="utf-8")
    env_path.write_text("ZENDESK_SUBDOMAIN=existing\n", encoding="utf-8")
    prompts: list[str] = []

    def fake_input(prompt: str) -> str:
        prompts.append(prompt)
        return "n"

    monkeypatch.setattr(script, "ENV_EXAMPLE_PATH", env_example)
    monkeypatch.setattr(script, "ENV_PATH", env_path)
    monkeypatch.setattr("builtins.input", fake_input)
    monkeypatch.setattr(
        script.getpass,
        "getpass",
        lambda prompt: pytest.fail(f"unexpected secret prompt: {prompt}"),
    )

    script.run_config()

    assert env_path.read_text(encoding="utf-8") == "ZENDESK_SUBDOMAIN=existing\n"
    assert len(prompts) == 1
    assert "re-configure" in prompts[0]


def test_verify_phase_runs_build_map_counts_entries_and_smokes_help(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    script = _load_script()
    commands: list[tuple[str, ...]] = []
    venv_dir_name = "Scripts" if sys.platform == "win32" else "bin"
    venv_scripts = tmp_path / ".venv" / venv_dir_name
    venv_scripts.mkdir(parents=True)
    script_name = "triage-cli.exe" if sys.platform == "win32" else "triage-cli"
    triage_cli = venv_scripts / script_name
    triage_cli.write_text("", encoding="utf-8")
    site_map_path = tmp_path / "data" / "cnc-map.json"

    def fake_run_capture(command: list[str]):
        commands.append(tuple(command))
        if command[-1] == "build-map":
            data_dir = site_map_path.parent
            data_dir.mkdir()
            site_map_path.write_text(
                json.dumps(
                    [
                        {
                            "friendly_name": f"Site {index}",
                            "site_name": f"site-{index}",
                            "cnc": f"00000000-0000-0000-0000-{index:012d}",
                        }
                        for index in range(30)
                    ]
                ),
                encoding="utf-8",
            )
            return script.CommandResult(returncode=0, output="built map\n")
        return script.CommandResult(
            returncode=0,
            output=(
                "Usage: triage-cli [OPTIONS] COMMAND [ARGS]...\n"
                "triage\ninvestigate\ninbox\nwatch\nsetup\ndoctor\nbuild-map\n"
            ),
        )

    monkeypatch.setattr(script, "VENV_PATH", tmp_path / ".venv")
    monkeypatch.setattr(script, "SITE_MAP_PATH", site_map_path)
    monkeypatch.setattr(script, "run_capture", fake_run_capture)

    script.run_verify()

    captured = capsys.readouterr()
    assert [command[-1] for command in commands] == ["build-map", "--help"]
    assert Path(commands[0][0]) == triage_cli
    assert Path(commands[1][0]) == triage_cli
    assert "30" in captured.out
    assert "help" in captured.out


def test_verify_phase_rejects_too_small_generated_map(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    script = _load_script()
    venv_dir_name = "Scripts" if sys.platform == "win32" else "bin"
    venv_scripts = tmp_path / ".venv" / venv_dir_name
    venv_scripts.mkdir(parents=True)
    script_name = "triage-cli.exe" if sys.platform == "win32" else "triage-cli"
    (venv_scripts / script_name).write_text("", encoding="utf-8")
    site_map_path = tmp_path / "data" / "cnc-map.json"

    def fake_run_capture(command: list[str]):
        if command[-1] == "build-map":
            data_dir = site_map_path.parent
            data_dir.mkdir()
            site_map_path.write_text("[]", encoding="utf-8")
        return script.CommandResult(returncode=0, output="")

    monkeypatch.setattr(script, "VENV_PATH", tmp_path / ".venv")
    monkeypatch.setattr(script, "SITE_MAP_PATH", site_map_path)
    monkeypatch.setattr(script, "run_capture", fake_run_capture)

    with pytest.raises(script.SetupError, match="30"):
        script.run_verify()


def test_verify_phase_rejects_help_missing_inbox_subcommand(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    script = _load_script()
    venv_dir_name = "Scripts" if sys.platform == "win32" else "bin"
    venv_scripts = tmp_path / ".venv" / venv_dir_name
    venv_scripts.mkdir(parents=True)
    script_name = "triage-cli.exe" if sys.platform == "win32" else "triage-cli"
    (venv_scripts / script_name).write_text("", encoding="utf-8")
    site_map_path = tmp_path / "data" / "cnc-map.json"

    def fake_run_capture(command: list[str]):
        if command[-1] == "build-map":
            site_map_path.parent.mkdir()
            site_map_path.write_text(
                json.dumps([{"site_name": f"site-{index}"} for index in range(30)]),
                encoding="utf-8",
            )
            return script.CommandResult(returncode=0, output="")
        return script.CommandResult(
            returncode=0,
            output="triage\ninvestigate\nwatch\nsetup\ndoctor\nbuild-map\n",
        )

    monkeypatch.setattr(script, "VENV_PATH", tmp_path / ".venv")
    monkeypatch.setattr(script, "SITE_MAP_PATH", site_map_path)
    monkeypatch.setattr(script, "run_capture", fake_run_capture)

    with pytest.raises(script.SetupError, match="inbox"):
        script.run_verify()
