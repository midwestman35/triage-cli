# Testing Guide

This guide covers all test tiers for triage-cli and how to set up a sandbox for agent-dispatched end-to-end testing.

## Quick Reference

| Tier | Command | Network | Credentials | CI-safe |
|------|---------|---------|-------------|---------|
| Unit | `cargo test --lib` | No | No | Yes |
| Integration | `cargo test --test integration` | No | No | Yes |
| CLI Smoke | `cargo test --test integration runbook_cli` | No | No | Yes (needs release build) |
| Sandbox (live) | `SANDBOX_INTEGRATION=1 cargo test --test sandbox -- --nocapture` | Yes | Yes | No |
| Codex Contract | `CODEX_AVAILABLE=1 cargo test --test codex_contract -- --nocapture` | Yes | Yes | No |

## Running All Offline Tests

```bash
cd triage-cli-rs
cargo build --release        # needed for CLI smoke tests
cargo test --lib             # inline unit tests
cargo test --test integration  # offline integration tests + CLI smoke tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

No `.env`, no credentials, no network required.

## Live Sandbox Tests

These tests exercise real Zendesk API calls and verify end-to-end workflows matching the runbooks in `docs/runbooks/`.

### Prerequisites

1. A `.env` file in the repo root with valid credentials:
   ```
   ZENDESK_SUBDOMAIN=yourcompany
   ZENDESK_EMAIL=agent@yourcompany.com
   ZENDESK_API_TOKEN=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx
   LLM_PROVIDER=unleash
   UNLEASH_API_KEY=xxxxxxxx
   UNLEASH_ASSISTANT_ID=xxxxxxxx
   ```
2. Run `triage-cli doctor` to verify — it should exit 0.

### Running

```bash
# From the repo root:
set -a && source .env && set +a
SANDBOX_INTEGRATION=1 cargo test --test sandbox -- --nocapture
```

All sandbox tests skip unless `SANDBOX_INTEGRATION=1` is set, so they're safe to leave in the test suite for normal `cargo test` runs.

### What the sandbox tests verify

| Test | Runbook | What it checks |
|------|---------|---------------|
| `live_triage_assigned_queue_ticket` | 02 | Fetches a real ticket from your assigned Zendesk queue, runs `investigate_one_structured` with `--no-llm`, verifies all 5 markdown files are written |
| `live_doctor_passes_with_valid_env` | 01, 05 | Runs `doctor` with real credentials, verifies all checks pass |

### Dispatching agents to a sandbox

To dispatch a Codex Cloud agent that can run these tests:

1. **Pre-configure the sandbox environment:**
   ```bash
   # Run setup interactively OR write .env directly:
   triage-cli setup
   # Verify:
   triage-cli doctor
   ```

2. **Add these environment variables to the agent's session:**
   ```bash
   export SANDBOX_INTEGRATION=1
   ```

3. **Run the full test suite:**
   ```bash
   cd triage-cli-rs
   cargo build --release
   cargo test --lib
   cargo test --test integration
   SANDBOX_INTEGRATION=1 cargo test --test sandbox -- --nocapture
   ```

4. **For targeted runbook testing:**
   ```bash
   # Only runbook 02 (triage)
   cargo test --test integration runbook_02
   
   # Only runbook 06 (watcher state logic)
   cargo test --test integration runbook_06
   
   # Only CLI smoke tests
   cargo test --test integration runbook_cli
   ```

## Test Architecture

### ZendeskSource Trait

The `ZendeskSource` trait (in `src/zendesk.rs`) mirrors the existing `DatadogSource` pattern:
- `ZendeskClient` implements it with real HTTP calls
- `ZendeskFixtureClient` (in `tests/integration/zendesk_mock.rs`) serves fixture data
- Pipeline and watcher accept `Option<&dyn ZendeskSource>` so tests inject mocks

### Fixture Data

Four named fixtures in `triage-cli-rs/fixtures/`:

| Fixture | Ticket # | Scenario |
|---------|----------|----------|
| `audio-drop` | 55001 | Audio drops on all dispatch consoles (post-update) |
| `no-site-map` | 55002 | Customer not in CNC site map |
| `missing-evidence` | 55004 | Insufficient evidence to fork |
| `vendor-fork` | 55003 | Vendor escalation scenario |

Each contains `ticket.json`, `datadog-logs.json` (optional), and `memory-hits.json` (optional).

### Key Patterns

- **`MemoryEnvScope`** — RAII guard that overrides `TRIAGE_MEMORY_MD`, `TRIAGE_MEMORY_DB`, and `TRIAGE_TICKETS_ROOT` to temp paths and restores on drop. All integration tests use this to avoid polluting real data.
- **`InvestigateOptions.tickets_root`** — Overrides where the five-markdown folder is written. Set to a tempdir in tests.
- **`InvestigateOptions.no_llm: true`** — Bypasses LLM call, produces deterministic stub output (fork D, low confidence).
- **`InvestigateOptions.force: true`** — Bypasses STATE.md soft-lock (needed in tests since temp dirs are fresh).

## Adding New Integration Tests

1. Create `tests/integration/runbook_XX_name.rs`
2. Add `mod runbook_XX_name;` to `tests/integration/mod.rs`
3. Use the `run_fixture_pipeline` helper or construct `ZendeskFixtureClient` directly
4. Run with `cargo test --test integration runbook_XX`

## Troubleshooting

- **`ZendeskSource` not found** — ensure `triage_cli::zendesk::ZendeskSource` is exported in `lib.rs`
- **`Ticket::default()` not available** — construct explicitly with required fields
- **`doctor` test fails** — env vars may leak between tests; use `MemoryEnvScope`
- **CLI smoke tests fail** — run `cargo build --release` first; the binary must exist at `target/release/triage-cli`
- **Sandbox tests all skip** — set `SANDBOX_INTEGRATION=1` in the environment
- **Flaky watcher state tests** — `should_triage` uses `MemoryEnvScope` for isolation; if still flaky, add `--test-threads=1`