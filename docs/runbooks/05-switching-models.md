# Switch the LLM provider or model

> **When to use this:** you want to move `triage`/`investigate`/`watch` LLM calls between the internal Unleash gateway and a local `codex` CLI subprocess, or pin a different codex model.

`src/llm.rs` reads `LLM_PROVIDER` and dispatches through the provider trait in `src/providers/mod.rs`. As of 2026-05-14 only two values are accepted:

| Provider | Env value | Required configuration | Default model |
| --- | --- | --- | --- |
| Unleash (default) | `unleash` | `UNLEASH_API_KEY`, `UNLEASH_ASSISTANT_ID`; optional `UNLEASH_BASE_URL`, `UNLEASH_ACCOUNT` | Selected server-side by the assistant ID — the CLI does not pass a model parameter. |
| Codex CLI | `codex` | `codex` binary on `PATH` + an existing codex OAuth session; optional `CODEX_MODEL` | `gpt-5-codex` |

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
   # Dev escape hatch — subprocess to the local codex CLI.
   LLM_PROVIDER=codex
   CODEX_MODEL=gpt-5-codex   # optional; this is the default
   ```

2. **Run a cheap triage smoke:**

   ```bash
   triage-cli triage <ticket-id> --no-logs
   ```

   `--no-logs` confirms the provider call without spending Datadog quota.

3. **If switching to codex, verify the local seat once:**

   ```bash
   codex exec --model gpt-5-codex "ping"
   ```

   You should see a non-empty response. If it fails, run `codex` once interactively to refresh the OAuth session.

## Verification

- The triage command exits `0` and writes the five-markdown ticket folder under `${TRIAGE_TICKETS_ROOT:-./Tickets}/<id>/`; `FORK_PACKET.md` also streams to stdout.
- Missing-env errors mention the missing variable before any network request is attempted (`UNLEASH_API_KEY must be set`, `codex CLI not found on PATH`, etc.).
- `triage-cli doctor` shows a green check for the selected provider.

## Troubleshooting

- **`UNLEASH_API_KEY must be set` / `UNLEASH_ASSISTANT_ID must be set`** — fill in Unleash credentials, or switch to `LLM_PROVIDER=codex`.
- **`codex CLI not found on PATH`** — install the `codex` CLI and ensure the binary is reachable. Confirm with `which codex`.
- **`codex exec` returns an OAuth error** — run `codex` interactively in the same shell to refresh the OAuth session, then retry the triage.
- **"Model not found" or 404** — typo in `CODEX_MODEL`, or the model is unavailable for the account. Cross-check with `codex exec --model <m> "ping"` directly before changing prompts.
- **Unknown `LLM_PROVIDER` value** — only `unleash` and `codex` are accepted; `doctor` will reject anything else.

## What was removed in 2026-05-14

The `claude` and `openai` providers were deleted. See `docs/adr/0002-prune-claude-openai-providers.md` for the rationale, the consequences, and the reversal path if either provider needs to be reintroduced.
