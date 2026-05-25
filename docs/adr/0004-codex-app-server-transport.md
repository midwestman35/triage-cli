# ADR 0004: Codex App-Server Transport with Exec Fallback

**Status**: Accepted  
**Date**: 2026-05-25  
**Branch**: feat/codex-app-server  

## Context

With `LLM_PROVIDER=codex`, triage-cli historically spawned `codex exec` for every LLM call. That subprocess contract is stable for structured JSON output (`triage_structured`, anchor/site extraction) but adds per-turn process startup and resume overhead for inbox chat follow-ups.

Codex CLI 0.130+ exposes a persistent `codex app-server --listen stdio://` JSON-RPC surface (`initialize`, `account/*`, `thread/start`, `thread/resume`, `turn/start`) suitable for resumable inbox turns and device-code setup without writing OAuth tokens into `.env`.

## Decision

1. **Follow-up transport env** — Add `CODEX_TRANSPORT` with values `app-server` (default when unset or unrecognized) and `exec` (explicit subprocess-only).

2. **Provider dispatch** (`providers/mod.rs`):
   - `CODEX_TRANSPORT=exec` → `CodexSubprocessProvider` only.
   - Otherwise → `CodexAppServerProvider` when a capability probe passes (`codex` on PATH, `app-server` subcommand, `initialize` succeeds).
   - If app-server is requested but unavailable → one-time stderr hint and fallback to `CodexSubprocessProvider` (same as setting `CODEX_TRANSPORT=exec`).

3. **Surface split (current v1)**:
   - `LlmProvider::followup` (inbox chat) uses app-server when selected.
   - `LlmProvider::complete` (`triage_structured`, `extract_anchor`, etc.) currently stays on `codex exec` subprocess, regardless of `CODEX_TRANSPORT`. Structured-output parity on app-server is deferred to v2.

4. **Structured `complete()` transport env (v2)** — Add `CODEX_COMPLETE_TRANSPORT=exec|app-server|auto`:
   - `exec` keeps the v1 subprocess path for `complete()`.
   - `app-server` routes structured `complete()` through app-server only after the structured parity gate is intentionally enabled.
   - `auto` is the safe default and currently remains `exec` until checked-in parity evidence is accepted. After that evidence lands, `auto` may prefer app-server when the app-server probe passes.
   - `CODEX_TRANSPORT=exec` remains a global rollback and forces subprocess behavior for Codex calls.

5. **Session provenance** — `SessionManifest` records `codex_transport` (`app-server` | `exec`) and `codex_capture_method`:
   - exec path: `codex_json_output` (unchanged; see `triage-cli-rs/docs/decisions/2026-05-17-codex-session-capture.md`).
   - app-server path: `app_server_thread_id` from `thread/start` / `thread/resume` (`thread.id` stored on `Turn.session_id`).
   - v2 structured `complete()` over app-server uses `app_server_output_schema`; by default those one-shot turns run on ephemeral app-server threads and do not update ticket `SessionManifest` / `codex_thread_id`.
   - Exec and app-server thread IDs are **not** interchangeable; mixed transport on one ticket follows existing provider-mismatch / replay rules.

6. **Setup / doctor** — `setup` is async; for Codex it may run ChatGPT device-code login via app-server (`account/login/start` → user visits URL → `account/login/completed`). `doctor` probes app-server auth read-only and does not start login. Setup writes `CODEX_TRANSPORT=app-server` when the probe passes, else `exec` with a yellow hint.

## Consequences

- Operators get lower inbox latency and streamed progress when app-server works; `CODEX_TRANSPORT=exec` is the safe rollback without changing `LLM_PROVIDER`.
- CI and offline `cargo test` should default `CODEX_TRANSPORT=exec` or `CODEX_COMPLETE_TRANSPORT=exec` (or omit app-server probes) so builds do not require a live Codex seat.
- Optional `CODEX_AVAILABLE=1` jobs run `tests/codex_contract.rs` (exec) and `tests/codex_app_server_contract.rs` (app-server smoke).
- v2 may route `complete()` through the shared app-server client once structured turns are validated; until then `CODEX_COMPLETE_TRANSPORT=auto` is intentionally equivalent to `exec`.
- Cancel semantics are transport-specific: app-server can issue server-side `turn/interrupt` once v2 Track 3 lands; exec cancel is local-only unless a separate process-kill task is implemented.

## References

- Implementation plan: `docs/superpowers/plans/2026-05-25-codex-app-server-transition.md`
- Exec capture decision: `triage-cli-rs/docs/decisions/2026-05-17-codex-session-capture.md`
- Code: `triage-cli-rs/src/providers/codex_app_server.rs`, `triage-cli-rs/src/providers/mod.rs`, `triage-cli-rs/src/setup.rs`
