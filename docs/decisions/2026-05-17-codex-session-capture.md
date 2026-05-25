# Codex Session-ID Capture (2026-05-17, updated 2026-05-25)

**Status:** Decided  
**Canonical detail:** [`triage-cli-rs/docs/decisions/2026-05-17-codex-session-capture.md`](../../triage-cli-rs/docs/decisions/2026-05-17-codex-session-capture.md) (exec JSONL contract, acceptance evidence, session-expired surface).

**Architecture:** [ADR 0004 — Codex app-server transport](../adr/0004-codex-app-server-transport.md)

## Exec path (unchanged for structured `complete()` in v1)

Initial `investigate` / `triage` and all `LlmProvider::complete` calls (`triage_structured`, anchor/site extraction) use **`CODEX_TRANSPORT=exec`** semantics: subprocess `codex exec --json`, capture method **`codex_json_output`** (first JSONL record with `type == "thread.started"`, field `thread_id` → stored as `Turn.session_id`).

This remains the v1 contract even when inbox follow-ups use app-server. See the canonical doc for parsing rules, contract tests (`tests/codex_contract.rs`), and resume failure handling.

## App-server path (inbox `followup` when `CODEX_TRANSPORT` is not `exec`)

When `LLM_PROVIDER=codex` and app-server is active (`CODEX_TRANSPORT` default or `app-server`, probe passes):

- **Capture method label:** `app_server_thread_id`
- **Source:** JSON-RPC `thread/start` or `thread/resume` → `thread.id` on the provider turn
- **Manifest:** `SessionManifest.codex_thread_id`, `codex_transport: app-server`, `codex_capture_method: app_server_thread_id`
- **Not interchangeable** with exec-captured UUIDs on the same ticket if `codex_transport` in the manifest does not match the active transport

Contract smoke: `CODEX_AVAILABLE=1 cargo test --test codex_app_server_contract -- --nocapture`

## Operator summary

| Surface | Transport | Capture method |
| --- | --- | --- |
| `complete()` (structured triage, anchors) | exec subprocess | `codex_json_output` |
| `followup()` (inbox chat) | app-server when capable, else exec | `app_server_thread_id` or `codex_json_output` |

Rollback: set `CODEX_TRANSPORT=exec` in `.env` (documented in README and runbook 05).
