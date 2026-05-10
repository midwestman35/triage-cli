# Setup Script Design

**Date:** 2026-05-08
**Status:** Approved

**2026-05-10 update:** the setup engine now lives in `triage_cli/setup.py`
so the installed CLI can expose `triage-cli setup`. The bootstrap script remains
available at `scripts/setup.py` for fresh clones before the console command
exists.

## Goal

Replace the manual `docs/runbooks/01-first-time-setup.md` steps with an interactive `scripts/setup.py` that guides both new team members and experienced teammates through first-time setup and re-runs safely.

## Decisions

| Question | Decision | Reason |
|----------|----------|--------|
| Audience | Both new and experienced users | New users need guidance; teammates need speed |
| Form | Shared stdlib-only engine plus bootstrap script | Fresh clones need `scripts/setup.py`; installed/rerun flows can use `triage-cli setup` |
| Approach | Phase-based with resume | Clear progress, survives interruption, fast re-runs |
| `.env` config | Interactive prompts with validation | Catches credential mistakes early |
| Idempotency | Detect existing state, ask before overwriting | Safe re-runs without silent clobbering |

## Architecture

Shared engine: `triage_cli/setup.py`. It uses only the standard library so it
can be imported before dependencies are installed. Fresh clone bootstrap remains
runnable as:

```bash
python3.11 scripts/setup.py
```

Once the console command exists, rerun the same flow as:

```bash
triage-cli setup
```

### Phases

Four sequential phases. Each phase is checkpointed on success. Re-runs resume from the first incomplete phase.

```
PREREQS → ENVIRONMENT → CONFIG → VERIFY
```

| Phase | Runbook steps | Description |
|-------|--------------|-------------|
| `PREREQS` | Step 1 | Verify `python3.11` and the configured LLM provider prerequisites |
| `ENVIRONMENT` | Steps 2–4 | Create `.venv`, `ensurepip`, install `.[dev]` editable |
| `CONFIG` | Step 5 | Copy `.env.example` → `.env`, prompt each key with validation |
| `VERIFY` | Steps 6–7 | Run `triage-cli build-map`, smoke-test `triage-cli --help` |

### Checkpoint File

`.setup-state.json` at the project root. Written after each phase completes. Read on startup to determine resume point.

```json
{
  "completed_phases": ["PREREQS", "ENVIRONMENT"],
  "setup_version": "2"
}
```

On re-run, the phase header displays status for all four phases before execution begins:

```
  [✓] PREREQS      python3.11 · claude
  [✓] ENVIRONMENT  .venv · pip install
  [→] CONFIG       resuming here...
  [ ] VERIFY
```

## CONFIG Phase Detail

Keys are discovered from `.env.example` at runtime. Prompt behavior per key:

| Key | Behavior |
|-----|----------|
| `ZENDESK_SUBDOMAIN` | Required. Strips `https://` and trailing `/` if pasted. Validates no spaces. |
| `ZENDESK_EMAIL` | Required. Validates `@` present, strips whitespace. |
| `ZENDESK_API_TOKEN` | Required. Masked input via `getpass`. Validates non-empty, strips whitespace. |
| `DD_API_KEY` / `DD_APP_KEY` | Optional. Prompt shows `[optional, Enter to skip]`. Blank accepted. |
| `DD_SITE` / `DD_CALL_CENTER_TAG` / `DD_STATION_TAG` | Pre-filled from `.env.example` defaults. Enter to accept, or type new value. |
| `ANTHROPIC_MODEL` | Pre-filled with `claude-sonnet-4-6`. User can override. |

Invalid input repeats only the failing field with an inline error — no phase restart.

If `.env` already exists: `  .env already exists — re-configure it? [y/N]`. Answering N skips the entire CONFIG phase.

## VERIFY Phase Detail

Runs via `subprocess`:

1. `triage-cli build-map` — validates `data/cnc-map.json` has ≥ 30 entries, prints count on success.
2. `triage-cli --help` — smoke-test that the installed entry point works and all subcommands are listed.

The read-only queue verification (runbook step 7 / `08-read-only-my-queue-flow.md`) is **not** automated — it requires a live assigned Zendesk ticket. The script ends with a printed reminder pointing to that runbook.

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Missing prerequisite (`python3.11`, `claude`) | Print install instructions from runbook troubleshooting, exit code 1, phase not marked complete |
| pip install failure | Stream output to terminal, exit code 1, phase not marked complete — re-run retries ENVIRONMENT |
| Subprocess failure in VERIFY | Print failing command + output, mark phase incomplete, suggest running manually |
| Keyboard interrupt (Ctrl-C) | Print `\n  Setup paused. Re-run to resume.`, exit 0, checkpoint preserved |

## Platform Notes

The script is cross-platform Python. It must handle the venv path difference:
- POSIX: `.venv/bin/python`, `.venv/bin/pip`
- Windows: `.venv\Scripts\python.exe`, `.venv\Scripts\pip.exe`

All subprocess calls use the venv-local Python/pip paths directly (no shell activation required).

## Out of Scope

- Automated read-only queue verification (requires live Zendesk ticket)
- CI/CD use — this is a developer setup tool only
