"""Shared setup engine for triage-cli onboarding.

This module intentionally uses only the standard library so it can be imported
by both the installed CLI and the bootstrap script.
"""

from __future__ import annotations

import base64
import getpass
import json
import os
import shutil
import subprocess
import sys
import urllib.error
import urllib.request
from collections.abc import Callable
from enum import StrEnum
from pathlib import Path
from typing import NamedTuple

SETUP_VERSION = "2"
MIN_SITE_MAP_ENTRIES = 30

ROOT = Path(__file__).resolve().parents[1]
STATE_PATH = ROOT / ".setup-state.json"
ENV_EXAMPLE_PATH = ROOT / ".env.example"
ENV_PATH = ROOT / ".env"
VENV_PATH = ROOT / ".venv"
SITE_MAP_PATH = ROOT / "data" / "cnc-map.json"
TRIAGE_NOTES_PATH = ROOT / "triage-notes"
DATA_PATH = ROOT / "data"


class Phase(StrEnum):
    PREREQS = "PREREQS"
    ENVIRONMENT = "ENVIRONMENT"
    CONFIG = "CONFIG"
    VERIFY = "VERIFY"


PHASES = [Phase.PREREQS, Phase.ENVIRONMENT, Phase.CONFIG, Phase.VERIFY]


class CommandResult(NamedTuple):
    returncode: int
    output: str


class DoctorCheck(NamedTuple):
    status: str
    name: str
    detail: str


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


def doctor_main(*, zendesk_probe: bool = True) -> int:
    """Print a one-screen local readiness checklist."""
    checks = run_doctor_checks(zendesk_probe=zendesk_probe)
    failed = any(check.status == "fail" for check in checks)
    for check in checks:
        marker = {"ok": "[x]", "warn": "[!]", "fail": "[x]"}[check.status]
        print(f"{marker} {check.name}: {check.detail}")
    return 1 if failed else 0


def run_doctor_checks(*, zendesk_probe: bool = True) -> list[DoctorCheck]:
    file_values = read_env_values(ENV_PATH) if ENV_PATH.exists() else {}
    env_values = {**file_values, **os.environ}
    checks = [
        check_python_version(),
        check_env_present(),
        check_required_env(env_values),
        check_writable_dir(TRIAGE_NOTES_PATH, "triage-notes writable"),
        check_writable_dir(DATA_PATH, "data dir writable"),
        check_datadog_config(env_values),
        check_llm_provider(env_values),
    ]
    if zendesk_probe:
        checks.append(check_zendesk_probe(env_values))
    else:
        checks.append(DoctorCheck("warn", "zendesk probe", "skipped by flag"))
    return checks


def check_python_version() -> DoctorCheck:
    version = sys.version_info
    label = f"{version.major}.{version.minor}.{version.micro}"
    if version >= (3, 11):
        return DoctorCheck("ok", "python", label)
    return DoctorCheck("fail", "python", f"{label}; Python 3.11+ is required")


def check_env_present() -> DoctorCheck:
    if ENV_PATH.exists():
        return DoctorCheck("ok", ".env", f"found at {ENV_PATH}")
    return DoctorCheck("fail", ".env", f"missing at {ENV_PATH}")


def check_required_env(env_values: dict[str, str]) -> DoctorCheck:
    required = ("ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN")
    missing = [name for name in required if not env_values.get(name)]
    if missing:
        return DoctorCheck("fail", "zendesk env", "missing " + ", ".join(missing))
    return DoctorCheck("ok", "zendesk env", "required variables are present")


def check_writable_dir(path: Path, name: str) -> DoctorCheck:
    try:
        path.mkdir(parents=True, exist_ok=True)
        probe = path / ".doctor-write-test"
        probe.write_text("ok\n", encoding="utf-8")
        probe.unlink(missing_ok=True)
    except OSError as exc:
        return DoctorCheck("fail", name, str(exc))
    return DoctorCheck("ok", name, str(path))


def check_datadog_config(env_values: dict[str, str]) -> DoctorCheck:
    missing = [name for name in ("DD_API_KEY", "DD_APP_KEY") if not env_values.get(name)]
    if missing:
        return DoctorCheck(
            "warn",
            "datadog env",
            "optional enrichment missing " + ", ".join(missing),
        )
    return DoctorCheck("ok", "datadog env", "optional enrichment configured")


def check_llm_provider(env_values: dict[str, str]) -> DoctorCheck:
    provider = (env_values.get("LLM_PROVIDER") or "claude").strip().lower()
    if provider == "unleash":
        missing = [
            name for name in ("UNLEASH_API_KEY", "UNLEASH_ASSISTANT_ID")
            if not env_values.get(name)
        ]
        if missing:
            return DoctorCheck("fail", "llm provider", "missing " + ", ".join(missing))
        return DoctorCheck("ok", "llm provider", "unleash configured")
    if provider == "claude":
        if shutil.which("claude") is None:
            return DoctorCheck("fail", "llm provider", "claude not found on PATH")
        return DoctorCheck("ok", "llm provider", "claude found on PATH")
    if provider in {"openai", "codex"}:
        if not env_values.get("OPENAI_API_KEY"):
            return DoctorCheck("fail", "llm provider", "missing OPENAI_API_KEY")
        return DoctorCheck("ok", "llm provider", f"{provider} configured")
    return DoctorCheck("fail", "llm provider", f"unsupported LLM_PROVIDER={provider}")


def check_zendesk_probe(env_values: dict[str, str]) -> DoctorCheck:
    required = ("ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN")
    if any(not env_values.get(name) for name in required):
        return DoctorCheck("fail", "zendesk probe", "skipped because Zendesk env is incomplete")

    subdomain = env_values["ZENDESK_SUBDOMAIN"].strip()
    email = env_values["ZENDESK_EMAIL"].strip()
    token = env_values["ZENDESK_API_TOKEN"].strip()
    url = f"https://{subdomain}.zendesk.com/api/v2/users/me.json"
    auth = base64.b64encode(f"{email}/token:{token}".encode()).decode("ascii")
    request = urllib.request.Request(
        url,
        headers={
            "Authorization": f"Basic {auth}",
            "Accept": "application/json",
            "User-Agent": "triage-cli/0.1",
        },
        method="GET",
    )
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            if 200 <= response.status < 300:
                return DoctorCheck("ok", "zendesk probe", "/users/me.json succeeded")
            return DoctorCheck("fail", "zendesk probe", f"HTTP {response.status}")
    except urllib.error.HTTPError as exc:
        return DoctorCheck("fail", "zendesk probe", f"HTTP {exc.code}")
    except urllib.error.URLError as exc:
        return DoctorCheck("fail", "zendesk probe", str(exc.reason))


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
        Phase.PREREQS: "python3.11 / LLM provider",
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
    provider = configured_llm_provider()
    commands = ["python3.11"]
    if provider == "claude":
        commands.append("claude")

    for command in commands:
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
        ]
        if "claude" in missing:
            details.extend(
                [
                    "Install Claude Code, then run `claude` once interactively to "
                    "complete OAuth.",
                    "Claude is only required when LLM_PROVIDER=claude. Production "
                    "Unleash usage does not need a local LLM CLI.",
                ]
            )
        raise SetupError("\n".join(details))

    if provider == "claude":
        print("  python3.11 and claude are available.")
    else:
        print("  python3.11 is available; Unleash does not require a local LLM CLI.")


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
        values[key] = prompt_env_value(key, default, values)

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

    expected_subcommands = (
        "investigate",
        "triage",
        "inbox",
        "watch",
        "setup",
        "doctor",
        "build-map",
    )
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


def configured_llm_provider() -> str:
    env_provider = os.environ.get("LLM_PROVIDER")
    if env_provider:
        return env_provider.strip().lower() or "unleash"
    if ENV_PATH.exists():
        values = read_env_values(ENV_PATH)
        provider = values.get("LLM_PROVIDER")
        if provider:
            return provider.strip().lower() or "unleash"
    return "unleash"


def _selected_provider(values: dict[str, str] | None) -> str:
    if values and values.get("LLM_PROVIDER"):
        return values["LLM_PROVIDER"].strip().lower() or "unleash"
    return configured_llm_provider()


def prompt_env_value(
    key: str,
    default: str,
    values: dict[str, str] | None = None,
) -> str:
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
    if key == "LLM_PROVIDER":
        return prompt_until_valid(
            key,
            prompt=f"{key} [{default or 'unleash'}]: ",
            validator=validate_llm_provider,
            secret=False,
        )
    if key == "UNLEASH_API_KEY":
        if _selected_provider(values) == "unleash":
            return prompt_until_valid(
                key,
                prompt=f"{key}: ",
                validator=validate_required_secret,
                secret=True,
            )
        return input(f"{key} [optional, Enter to skip]: ").strip()
    if key == "UNLEASH_ASSISTANT_ID":
        if _selected_provider(values) == "unleash":
            return prompt_until_valid(
                key,
                prompt=f"{key}: ",
                validator=validate_required_secret,
                secret=False,
            )
        return input(f"{key} [optional, Enter to skip]: ").strip()
    if key == "UNLEASH_ACCOUNT":
        return input(f"{key} [optional, Enter to skip]: ").strip()
    if key in {
        "DD_SITE",
        "DD_CALL_CENTER_TAG",
        "DD_STATION_TAG",
        "UNLEASH_BASE_URL",
        "ANTHROPIC_MODEL",
    }:
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


def validate_llm_provider(raw_value: str) -> str:
    value = raw_value.strip().lower() or "unleash"
    if value not in {"unleash", "claude"}:
        raise ValueError("must be unleash or claude")
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
