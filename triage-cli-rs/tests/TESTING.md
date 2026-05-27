# Testing — triage-cli-rs

## Quick Reference

| Tier | Command | Network | Credentials | CI-safe |
|------|---------|---------|-------------|---------|
| Unit | `cargo test --lib` | No | No | Yes |
| Integration | `cargo test --test integration` | No | No | Yes |
| CLI Smoke | `cargo test --test integration runbook_cli` | No | No | Yes (needs release build) |
| Sandbox (live) | `SANDBOX_INTEGRATION=1 cargo test --test sandbox -- --nocapture` | Yes | Yes | No |
| Codex Contract (exec) | `CODEX_AVAILABLE=1 cargo test --test codex_contract -- --nocapture` | Yes | Yes | No |
| Codex App-Server Contract | `CODEX_AVAILABLE=1 cargo test --test codex_app_server_contract -- --nocapture` | Yes | Yes | No |

## Running All Offline Tests

```bash
cd triage-cli-rs
cargo build --release          # needed for CLI smoke tests
cargo test --lib               # inline unit tests
cargo test --test integration  # offline integration tests + CLI smoke tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

No `.env`, no credentials, no network required.

## CI and Codex transport

Default CI jobs should **not** set `CODEX_AVAILABLE=1` and should prefer
`CODEX_TRANSPORT=exec` (or `LLM_PROVIDER=unleash`) so builds never require a
live Codex seat or app-server probe.

Optional maintainer / nightly job when a Codex CLI is installed and authenticated:

```bash
export CODEX_AVAILABLE=1
export CODEX_TRANSPORT=app-server   # or exec for subprocess-only contract tests
CODEX_AVAILABLE=1 cargo test --test codex_contract -- --nocapture --test-threads=1
CODEX_AVAILABLE=1 cargo test --test codex_app_server_contract -- --nocapture
```

Offline regression gates for Codex code changes remain:
`cargo test --lib`, `cargo test --test pipeline_integration`,
`cargo clippy --all-targets -- -D warnings`.

## Operator-visible evidence tests

When a task claims a workflow is fixed or improved, add a test that proves what
the Trader / Operator / analyst now sees, receives, or can do. The assertion
should name:

1. **Before / trigger** — fixture, flag, prior state, or regression.
2. **Now / surface** — stdout, stderr, one of the five markdown files, metrics
   JSON, base-evidence manifest, session log, or rendered TUI state.
3. **Proof** — exact string, count, ID, schema version, path, fork letter,
   rubric row, or manifest body snippet.

Current example: `triage_fixture_writes_operator_visible_base_evidence_delta`
runs the `audio-drop` fixture with `--no-llm`, reads
`.session/base-evidence-manifest.json`, and proves the operator can inspect:

- schema `triage-cli/base-evidence`, `schema_version == 2`;
- two Zendesk comment bodies (`E-001`, `E-002`);
- one Datadog log-window body (`E-003`) with all 8 fixture log lines and the
  codec-mismatch signal.

## Live Sandbox Tests

Gated behind `SANDBOX_INTEGRATION=1`. Requires a `.env` in the repo root with valid Zendesk credentials. Run `triage-cli doctor` first to verify setup.

```bash
SANDBOX_INTEGRATION=1 cargo test --test sandbox -- --nocapture
```

| Test | What it checks |
|------|----------------|
| `live_triage_assigned_queue_ticket` | Fetches a real ticket, runs `--no-llm` pipeline, verifies 5 markdown files |
| `live_doctor_passes_with_valid_env` | Invokes `triage-cli doctor` subprocess, verifies exit 0 |

## CLI Smoke Tests

Subprocess tests that invoke the built `triage-cli` binary directly. Require `cargo build --release` first.

| Test | Command | Asserts |
|------|---------|---------|
| `runbook_02_cli_demo_audio_drop` | `triage-cli demo audio-drop` | exits 0 |
| `runbook_01_cli_doctor_flags_missing_env` | `triage-cli doctor` (empty env) | exits non-zero |
| `runbook_06_cli_watch_help` | `triage-cli watch --help` | exits 0, mentions `--view` |
| `runbook_07_cli_inbox_help` | `triage-cli inbox --help` | exits 0, mentions `--view` |
| `runbook_02_cli_triage_fixture` | `triage-cli triage 55001 --fixture ... --no-llm --force` | exits 0, stdout contains fork info |
| `runbook_02_cli_triage_fixture_isolated_home_preserves_datadog_metrics` | `triage-cli triage 55001 --fixture ... --no-llm --force --metrics-out ...` | runs under an empty isolated `TRIAGE_HOME`, skips `build-map`, metrics JSON reports `evidence_counts.datadog_lines == 8` |
| `runbook_03_cli_build_map` | `triage-cli build-map` | exits 0, mentions `cnc-map.json` |

Fixture CLI smoke tests intentionally isolate `TRIAGE_HOME` to a fresh tempdir.
The `runbook_02_cli_triage_fixture_isolated_home_preserves_datadog_metrics`
case does not copy `apex-cnc-inventory.md`, does not run `build-map`, and uses
`--metrics-out` JSON inspection to catch regressions where fixture Datadog
evidence is dropped to `datadog_lines: 0`.

## Test Architecture

### Three-Tier Strategy

| Tier | Network | Credentials | When |
|------|---------|-------------|------|
| Unit (`--lib`) | No | No | Always |
| Integration (`--test integration`) | No | No | Always |
| Sandbox (`--test sandbox`) | Yes | Yes | `SANDBOX_INTEGRATION=1` |

### ZendeskSource Trait

- `ZendeskClient` — real HTTP calls (used by sandbox tests)
- `ZendeskFixtureClient` — serves fixture data (used by offline integration tests)
- Pipeline accepts `Option<&dyn ZendeskSource>` for dependency injection

### Fixture Data

Four named fixtures in `triage-cli-rs/fixtures/`: `audio-drop`, `no-site-map`, `missing-evidence`, `vendor-fork`.

### Key Patterns

- **EnvGuard** — RAII guard in integration and sandbox tests that overrides `TRIAGE_HOME`, `TRIAGE_MEMORY_MD`, `TRIAGE_MEMORY_DB`, `TRIAGE_TICKETS_ROOT` to temp paths.
- **`InvestigateOptions.tickets_root`** — Overrides ticket folder write destination.
- **`--no-llm`** — Bypasses LLM call, produces deterministic stub output.
- **`--force`** — Bypasses STATE.md soft-lock.

### File Layout

```
tests/
  README.md                     # Detailed testing guide
  TESTING.md                    # Quick testing guide and proof patterns
  codex_contract.rs             # Codex exec JSONL contract (CODEX_AVAILABLE=1)
  codex_app_server_contract.rs  # App-server initialize smoke (CODEX_AVAILABLE=1)
  integration/
    mod.rs                      # Module root
    zendesk_mock.rs             # ZendeskFixtureClient
    runbook_01_setup.rs         # Doctor + build-map
    runbook_02_triage.rs        # Fixture-based triage pipeline + operator-visible evidence proof
    runbook_03_sitemap.rs       # Site map generation
    runbook_05_llm.rs           # LLM provider contract tests
    runbook_cli_smoke.rs        # CLI subprocess smoke tests
  sandbox/
    mod.rs                      # Module root (env helpers + gate)
    runbook_02_live_triage.rs   # Live Zendesk triage
    runbook_05_live_provider.rs # Live doctor check
```

## Dispatching Agents

1. Pre-configure sandbox: `triage-cli setup` or write `.env` directly.
2. Verify: `triage-cli doctor` exits 0.
3. Run: `SANDBOX_INTEGRATION=1 cargo test --test sandbox -- --nocapture`.

Full details: [`tests/README.md`](README.md).
