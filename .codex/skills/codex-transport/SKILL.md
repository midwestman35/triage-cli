---
name: codex-transport
description: Use when changing Codex provider code, app-server transport, setup/doctor probes, session capture, or Codex contract tests.
---

# Codex transport skill

Load this skill when a task touches Codex provider behavior or the CLI's Codex integration surface.

## Core doctrine

Codex is a development and inbox-followup transport, not the only production path. `unleash` remains the default provider. Codex behavior should be observable, gated in tests, and safe to run in CI without a live seat.

## Stable invariants

- `LLM_PROVIDER=codex` selects the Codex provider.
- `CODEX_TRANSPORT=app-server` is the persistent JSON-RPC path for inbox follow-ups when available.
- `CODEX_TRANSPORT=exec` is the subprocess fallback and CI-safe path.
- Structured `triage_structured` / anchor extraction should not silently switch transports without an ADR and parity evidence.
- `setup` and `doctor` probes must be read-only. They can check auth, initialize, account/model access, and fallback capability; they must not mutate ticket or provider state.
- Live Codex tests require `CODEX_AVAILABLE=1`; default offline tests must not require a Codex CLI, live auth, or network.
- Capture session/thread identifiers when the transport exposes them, but never require a resumable session to answer correctly; replay/context preambles are the safety net.

## Common files

- `triage-cli-rs/src/providers/mod.rs`
- `triage-cli-rs/src/providers/codex.rs`
- `triage-cli-rs/src/providers/codex_app_server.rs`
- `triage-cli-rs/src/llm.rs`
- `triage-cli-rs/src/setup.rs`
- `triage-cli-rs/tests/codex_contract.rs`
- `triage-cli-rs/tests/codex_app_server_contract.rs`
- `docs/adr/0004-codex-app-server-transport.md`

## Test evidence to require

- Offline regression gate: `cargo test --lib`, `cargo test --test pipeline_integration`, `cargo clippy --all-targets -- -D warnings`.
- Live gate only when relevant: `CODEX_AVAILABLE=1 cargo test --test codex_contract -- --nocapture --test-threads=1`.
- App-server live gate only when relevant: `CODEX_AVAILABLE=1 cargo test --test codex_app_server_contract -- --nocapture`.
- For transport changes, assert the visible fallback, captured session metadata, or provider metric rather than only checking that a call returned text.

## Keep this skill current

Update this file when adding a Codex transport, changing fallback policy, changing provider metrics, changing setup/doctor probes, or changing the live-test gate.
