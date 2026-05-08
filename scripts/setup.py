"""Interactive first-time setup for triage-cli.

This script intentionally uses only the standard library and does not import
triage_cli because it must run before the package is installed.
"""

from __future__ import annotations

import getpass
import json
import os
import shutil
import subprocess
import sys
from collections.abc import Callable
from enum import StrEnum
from pathlib import Path
from typing import NamedTuple

SETUP_VERSION = "1"
MIN_SITE_MAP_ENTRIES = 30

ROOT = Path(__file__).resolve().parents[1]
STATE_PATH = ROOT / ".setup-state.json"
ENV_EXAMPLE_PATH = ROOT / ".env.example"
ENV_PATH = ROOT / ".env"
VENV_PATH = ROOT / ".venv"
SITE_MAP_PATH = ROOT / "data" / "cnc-map.json"


class Phase(StrEnum):
    PREREQS = "PREREQS"
    ENVIRONMENT = "ENVIRONMENT"
    CONFIG = "CONFIG"
    VERIFY = "VERIFY"


PHASES = [Phase.PREREQS, Phase.ENVIRONMENT, Phase.CONFIG, Phase.VERIFY]


class CommandResult(NamedTuple):
    returncode: int
    output: str


def main() -> int:
    try:
        state = load_state()
        start_phase = first_incomplete_phase(state)
        print_phase_status(state, start_phase)

        runners: dict[Phase, Callable[[], None]] = {
            Phase.PREREQS: run_prereqs,
            Phase.ENVIRONMENT: run_environment,
            Phase.CONFIG: run_config,
            Phase.VERIFY: run_verify,
        }

        if start_phase is None:
            print("\nSetup is already complete.")
            print_readonly_reminder()
            return 0

        for phase in PHASES[PHASES.index(start_phase) :]:
            print(f"\n== {phase.value} ==")
            runners[phase]()
            mark_phase_complete(state, phase)

        print("\nSetup complete.")
        print_readonly_reminder()
        return 0
    except KeyboardInterrupt:
        print("\n  Setup paused. Re-run to resume.")
        return 0
    except SetupError as exc:
        print(f"\nSetup failed: {exc}", file=sys.stderr)
        return 1


class SetupError(RuntimeError):
    """Raised for user-actionable setup failures."""


def load_state() -> dict[str, object]:
    if not STATE_PATH.exists():
        return {"completed_phases": [], "setup_version": SETUP_VERSION}

    try:
        raw_state = json.loads(STATE_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise SetupError(f"could not read {STATE_PATH.name}: {exc}") from exc

    completed = raw_state.get("completed_phases", [])
    if not isinstance(completed, list):
        completed = []

    if raw_state.get("setup_version") != SETUP_VERSION:
        return {"completed_phases": [], "setup_version": SETUP_VERSION}

    valid_completed = [phase for phase in completed if phase in {item.value for item in PHASES}]
    return {"completed_phases": valid_completed, "setup_version": SETUP_VERSION}


def save_state(state: dict[str, object]) -> None:
    tmp_path = STATE_PATH.with_suffix(".json.tmp")
    tmp_path.write_text(json.dumps(state, indent=2) + "\n", encoding="utf-8")
    os.replace(tmp_path, STATE_PATH)


def mark_phase_complete(state: dict[str, object], phase: Phase) -> None:
    completed = list(state.get("completed_phases", []))
    if phase.value not in completed:
        completed.append(phase.value)
    state["completed_phases"] = completed
    state["setup_version"] = SETUP_VERSION
    save_state(state)


def first_incomplete_phase(state: dict[str, object]) -> Phase | None:
    completed = set(state.get("completed_phases", []))
    for phase in PHASES:
        if phase.value not in completed:
            return phase
    return None


def print_phase_status(state: dict[str, object], start_phase: Phase | None) -> None:
    completed = set(state.get("completed_phases", []))
    descriptions = {
        Phase.PREREQS: "python3.11 / claude",
        Phase.ENVIRONMENT: ".venv / pip install",
        Phase.CONFIG: ".env prompts",
        Phase.VERIFY: "build-map / help smoke",
    }

    print("Setup phases:")
    for phase in PHASES:
        if phase.value in completed:
            marker = "[x]"
            suffix = descriptions[phase]
        elif phase == start_phase:
            marker = "[>]"
            suffix = "resuming here..."
        else:
            marker = "[ ]"
            suffix = descriptions[phase]
        print(f"  {marker} {phase.value:<12} {suffix}")


def run_prereqs() -> None:
    missing: list[str] = []
    for command in ("python3.11", "claude"):
        executable = shutil.which(command)
        if executable is None:
            missing.append(command)
            continue
        result = run_capture([executable, "--version"])
        if result.returncode != 0:
            missing.append(command)

    if missing:
        details = [
            f"missing prerequisite(s): {', '.join(missing)}",
            "Install Python 3.11 and confirm `python3.11 --version` exits cleanly.",
            "Install Claude Code, then run `claude` once interactively to complete OAuth.",
            "The Agent SDK uses the Claude CLI session; there is no separate API key.",
        ]
        raise SetupError("\n".join(details))

    print("  python3.11 and claude are available.")


def run_environment() -> None:
    python_command = shutil.which("python3.11")
    if python_command is None:
        raise SetupError("python3.11 is not on PATH; re-run after installing Python 3.11.")

    if not VENV_PATH.exists():
        print("  Creating .venv...")
        run_checked_stream([python_command, "-m", "venv", str(VENV_PATH)])
    else:
        print("  Reusing existing .venv.")

    venv_python = venv_python_path()
    venv_pip = venv_pip_path()
    if not venv_python.exists():
        raise SetupError(f"expected venv Python at {venv_python}")

    print("  Ensuring pip is available...")
    run_checked_stream([str(venv_python), "-m", "ensurepip", "--upgrade"])

    if not venv_pip.exists():
        raise SetupError(f"expected venv pip at {venv_pip}")

    print("  Upgrading pip, setuptools, and wheel...")
    run_checked_stream([str(venv_pip), "install", "--upgrade", "pip", "setuptools", "wheel"])

    print('  Installing triage-cli in editable mode with dev extras...')
    run_checked_stream([str(venv_pip), "install", "-e", ".[dev]"])


def run_config() -> None:
    example_values = read_env_values(ENV_EXAMPLE_PATH)
    if not example_values:
        raise SetupError(f"{ENV_EXAMPLE_PATH.name} is missing or empty.")

    should_reconfigure = not ENV_PATH.exists() or ask_yes_no(
        "  .env already exists - re-configure it?",
        default=False,
    )
    if not should_reconfigure:
        print("  Keeping existing .env.")
        return

    values: dict[str, str] = {}
    for key, default in example_values.items():
        values[key] = prompt_env_value(key, default)

    lines = [f"{key}={value}" for key, value in values.items()]
    ENV_PATH.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("  Wrote .env.")


def run_verify() -> None:
    triage_cli = venv_script_path("triage-cli")
    if not triage_cli.exists():
        raise SetupError(
            f"expected installed console script at {triage_cli}; "
            "re-run setup to retry ENVIRONMENT."
        )

    build_result = run_capture([str(triage_cli), "build-map"])
    if build_result.returncode != 0:
        raise verify_error([str(triage_cli), "build-map"], build_result)

    count = site_map_entry_count()
    if count < MIN_SITE_MAP_ENTRIES:
        raise SetupError(
            f"{SITE_MAP_PATH} has {count} entries; expected at least {MIN_SITE_MAP_ENTRIES}."
        )
    print(f"  Site map contains {count} entries.")

    help_result = run_capture([str(triage_cli), "--help"])
    if help_result.returncode != 0:
        raise verify_error([str(triage_cli), "--help"], help_result)

    expected_subcommands = ("investigate", "triage", "inbox", "watch", "build-map")
    missing_subcommands = [name for name in expected_subcommands if name not in help_result.output]
    if missing_subcommands:
        raise SetupError(
            "`triage-cli --help` did not list expected subcommand(s): "
            + ", ".join(missing_subcommands)
            + "\nOutput:\n"
            + help_result.output
        )
    print("  triage-cli --help lists expected subcommands.")


def read_env_values(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        values[key.strip()] = value.strip()
    return values


def prompt_env_value(key: str, default: str) -> str:
    if key == "ZENDESK_SUBDOMAIN":
        return prompt_until_valid(
            key,
            prompt=f"{key}: ",
            validator=validate_zendesk_subdomain,
            secret=False,
        )
    if key == "ZENDESK_EMAIL":
        return prompt_until_valid(
            key,
            prompt=f"{key}: ",
            validator=validate_zendesk_email,
            secret=False,
        )
    if key == "ZENDESK_API_TOKEN":
        return prompt_until_valid(
            key,
            prompt=f"{key}: ",
            validator=validate_required_secret,
            secret=True,
        )
    if key in {"DD_API_KEY", "DD_APP_KEY"}:
        prompt = f"{key} [optional, Enter to skip]: "
        return input(prompt).strip()
    if key in {"DD_SITE", "DD_CALL_CENTER_TAG", "DD_STATION_TAG", "ANTHROPIC_MODEL"}:
        displayed_default = default or ("claude-sonnet-4-6" if key == "ANTHROPIC_MODEL" else "")
        answer = input(f"{key} [{displayed_default}]: ").strip()
        return answer or displayed_default

    answer = input(f"{key} [{default}]: ").strip()
    return answer or default


def prompt_until_valid(
    key: str,
    prompt: str,
    validator: Callable[[str], str],
    *,
    secret: bool,
) -> str:
    while True:
        raw_value = getpass.getpass(prompt) if secret else input(prompt)
        try:
            return validator(raw_value)
        except ValueError as exc:
            print(f"  {key}: {exc}")


def validate_zendesk_subdomain(raw_value: str) -> str:
    value = raw_value.strip()
    for prefix in ("https://", "http://"):
        if value.lower().startswith(prefix):
            value = value[len(prefix) :]
    value = value.rstrip("/")
    if not value:
        raise ValueError("required")
    if any(char.isspace() for char in value):
        raise ValueError("must not contain spaces")
    return value


def validate_zendesk_email(raw_value: str) -> str:
    value = raw_value.strip()
    if not value:
        raise ValueError("required")
    if "@" not in value:
        raise ValueError("must contain @")
    return value


def validate_required_secret(raw_value: str) -> str:
    value = raw_value.strip()
    if not value:
        raise ValueError("required")
    return value


def ask_yes_no(question: str, *, default: bool) -> bool:
    suffix = "[Y/n]" if default else "[y/N]"
    answer = input(f"{question} {suffix} ").strip().lower()
    if not answer:
        return default
    return answer in {"y", "yes"}


def venv_python_path() -> Path:
    if os.name == "nt":
        return VENV_PATH / "Scripts" / "python.exe"
    return VENV_PATH / "bin" / "python"


def venv_pip_path() -> Path:
    if os.name == "nt":
        return VENV_PATH / "Scripts" / "pip.exe"
    return VENV_PATH / "bin" / "pip"


def venv_script_path(script_name: str) -> Path:
    if os.name == "nt":
        return VENV_PATH / "Scripts" / f"{script_name}.exe"
    return VENV_PATH / "bin" / script_name


def run_checked_stream(command: list[str]) -> None:
    process = subprocess.Popen(command, cwd=ROOT)
    returncode = process.wait()
    if returncode != 0:
        raise SetupError(f"command failed with exit code {returncode}: {format_command(command)}")


def run_capture(command: list[str]) -> CommandResult:
    process = subprocess.run(
        command,
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    return CommandResult(returncode=process.returncode, output=process.stdout)


def verify_error(command: list[str], result: CommandResult) -> SetupError:
    return SetupError(
        "verification command failed; run it manually after fixing the issue.\n"
        f"Command: {format_command(command)}\n"
        f"Exit code: {result.returncode}\n"
        f"Output:\n{result.output}"
    )


def format_command(command: list[str]) -> str:
    return " ".join(command)


def site_map_entry_count() -> int:
    try:
        data = json.loads(SITE_MAP_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise SetupError(f"could not read {SITE_MAP_PATH}: {exc}") from exc
    if not isinstance(data, list):
        raise SetupError(f"{SITE_MAP_PATH} is not a JSON list.")
    return len(data)


def print_readonly_reminder() -> None:
    print(
        "Reminder: live read-only queue verification is not automated. "
        "Follow docs/runbooks/08-read-only-my-queue-flow.md when you have an assigned ticket."
    )


if __name__ == "__main__":
    raise SystemExit(main())
