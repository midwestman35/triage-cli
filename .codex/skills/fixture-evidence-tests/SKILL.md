---
name: fixture-evidence-tests
description: Use when changing fixtures, offline integration tests, metrics JSON, golden outputs, or operator-visible proof assertions.
---

# Fixture evidence tests skill

Load this skill when a task touches tests, fixtures, demo mode, or any proof that a change is visible to the analyst.

## Core doctrine

A passing test should prove the operator-visible delta. Prefer “given this fixture and flag, the analyst now sees/receives/can do X” over “the function returned Ok.”

## Stable invariants

- Fixture and demo runs are offline: no Zendesk credentials, no Datadog credentials, no provider credentials.
- `--no-llm` produces deterministic stub output for byte-stable testing.
- `--metrics-out` is best-effort and must not change the process exit code.
- Fixture runs may isolate `TRIAGE_HOME` to an empty tempdir; tests should catch regressions where fixture data is dropped because repo-local data is absent.
- Base evidence manifests are operator/debug proof surfaces. Schema/version, evidence IDs, source paths, counts, and body snippets are fair assertions.
- CLI smoke tests require a release binary; normal integration tests should exercise library paths without subprocess cost.

## Common files

- `triage-cli-rs/fixtures/**`
- `triage-cli-rs/src/fixture.rs`
- `triage-cli-rs/tests/integration/**`
- `triage-cli-rs/tests/pipeline_integration.rs`
- `triage-cli-rs/tests/TESTING.md`
- `triage-cli-rs/tests/README.md`

## Operator-visible proof pattern

Each new regression test should name or encode:

1. **Before / trigger** — fixture, flags, isolated env, existing folder state, or prior bug.
2. **Now / surface** — exact artifact: stdout, stderr, markdown file, metrics JSON, base-evidence manifest, session log, or rendered TUI state.
3. **Evidence** — exact string/count/ID/schema/path/body snippet proving the operator can inspect the change.

Good assertion examples:

- `metrics["evidence_counts"]["datadog_lines"] == 8`
- `base-evidence-manifest.json` has `schema_version == 2`
- evidence ID `E-003` has `kind == "datadog_log_window"` and a body containing `codec mismatch`
- `FORK_PACKET.md` stdout contains `Fork: D — Cannot fork yet`
- soft-lock failure leaves pre-existing `STATE.md` unchanged

## Gates

```bash
cd triage-cli-rs
cargo test --test integration
cargo test --test pipeline_integration
cargo test --lib
```

CLI smoke tests also need:

```bash
cargo build --release
cargo test --test integration runbook_cli
```

## Keep this skill current

Update this file when adding fixtures, changing fixture resolution, changing metrics shape, changing base-evidence manifest schema, or adding a new operator-visible proof surface.
