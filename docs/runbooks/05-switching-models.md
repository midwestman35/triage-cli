# Switch the LLM provider or model

> **When to use this:** you want to move `triage`/`investigate`/`watch` LLM calls between the internal Unleash gateway and Codex, pin a different codex model, or switch Codex transport (`app-server` vs `exec`).

`src/llm.rs` reads `LLM_PROVIDER` and dispatches through the provider trait in `src/providers/mod.rs`. As of 2026-05-14 only two provider values are accepted:

| Provider | Env value | Required configuration | Default model |
| --- | --- | --- | --- |
| Unleash (default) | `unleash` | `UNLEASH_API_KEY`, `UNLEASH_ASSISTANT_ID`; optional `UNLEASH_BASE_URL`, `UNLEASH_ACCOUNT` | Selected server-side by the assistant ID — the CLI does not pass a model parameter. |
| Codex CLI | `codex` | `codex` on `PATH`; `CODEX_TRANSPORT`; auth via `setup` (app-server) or `codex` OAuth (`exec`); optional `CODEX_MODEL` | `gpt-5.5` |

### Codex transport (`CODEX_TRANSPORT`)

| Value | When to use |
| --- | --- |
| `app-server` | Default. Inbox chat uses persistent `codex app-server`; `setup` / `doctor` use device-code auth. Structured `triage`/`investigate` still call `codex exec` in v1. |
| `exec` | CI, older Codex CLI without `app-server`, or rollback. All Codex calls use subprocess; doctor skips app-server auth probes. |

`setup` writes the appropriate value after probing. See `docs/adr/0004-codex-app-server-transport.md`.

## Steps

1. **Edit `.env`** and set the provider:

   ```bash
   $EDITOR .env
   ```

   Pick one of:

   ```dotenv
   # Production path — HTTP to the internal Axon gateway.
   LLM_PROVIDER=unleash
   UNLEASH_API_KEY=...
   UNLEASH_ASSISTANT_ID=...
   ```

   ```dotenv
   # Dev escape hatch — Codex CLI (app-server for inbox; setup handles auth).
   LLM_PROVIDER=codex
   CODEX_TRANSPORT=app-server
   CODEX_MODEL=gpt-5.5   # optional; this is the default
   ```

   ```dotenv
   # Subprocess-only (CI or rollback).
   LLM_PROVIDER=codex
   CODEX_TRANSPORT=exec
   CODEX_MODEL=gpt-5.5
   ```

2. **For Codex app-server, run setup once after editing `.env`:**

   ```bash
   triage-cli setup
   ```

   Completes device-code login when `account/read` is unauthenticated. Re-run `triage-cli doctor`.

3. **Run a cheap triage smoke:**

   ```bash
   triage-cli triage <ticket-id> --no-logs
   ```

   `--no-logs` confirms the provider call without spending Datadog quota.

4. **If using `CODEX_TRANSPORT=exec`, verify the subprocess seat once:**

   ```bash
   codex exec --model gpt-5.5 "ping"
   ```

   You should see a non-empty response. If it fails, run `codex` once interactively to refresh the OAuth session.

## Verification

- The triage command exits `0` and writes the five-markdown ticket folder under `${TRIAGE_TICKETS_ROOT:-./Tickets}/<id>/`; `FORK_PACKET.md` also streams to stdout.
- Missing-env errors mention the missing variable before any network request is attempted (`UNLEASH_API_KEY must be set`, `codex CLI not found on PATH`, etc.).
- `triage-cli doctor` shows a green check for the selected provider.

## Troubleshooting

- **`UNLEASH_API_KEY must be set` / `UNLEASH_ASSISTANT_ID must be set`** — fill in Unleash credentials, or switch to `LLM_PROVIDER=codex`.
- **`codex CLI not found on PATH`** — install the `codex` CLI and ensure the binary is reachable. Confirm with `which codex`.
- **`codex app-server` subcommand not available** — upgrade Codex CLI or set `CODEX_TRANSPORT=exec`.
- **`codex account not authenticated`** (doctor) — run `triage-cli setup` and complete device-code login; doctor does not start login by design.
- **`codex exec` returns an OAuth error** — only relevant when `CODEX_TRANSPORT=exec`; run `codex` interactively to refresh OAuth, then retry.
- **Inbox resume fails after transport change** — thread IDs from exec vs app-server are not interchangeable; see `docs/decisions/2026-05-17-codex-session-capture.md`.
- **"Model not found" or 404** — typo in `CODEX_MODEL`, or the model is unavailable for the account. Cross-check with `doctor` (`model/list`) or `codex exec --model <m> "ping"` when on `exec`.
- **Unknown `LLM_PROVIDER` value** — only `unleash` and `codex` are accepted; `doctor` will reject anything else.

## What was removed in 2026-05-14

The `claude` and `openai` providers were deleted. See `docs/adr/0002-prune-claude-openai-providers.md` for the rationale, the consequences, and the reversal path if either provider needs to be reintroduced.
