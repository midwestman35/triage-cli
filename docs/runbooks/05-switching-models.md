# Switch the LLM provider or model

> **When to use this:** you want to move `triage`/watcher LLM calls between Unleash, Claude Code, and OpenAI/Codex HTTP, or pin a different provider model.

`triage_cli/llm.py` reads `LLM_PROVIDER` and dispatches through a small provider protocol. Supported values:

| Provider | Env value | Required configuration |
| --- | --- | --- |
| Unleash | `unleash` | `UNLEASH_API_KEY`, `UNLEASH_ASSISTANT_ID`, optional `UNLEASH_BASE_URL`, optional `UNLEASH_ACCOUNT` |
| Claude Code | `claude` | optional `ANTHROPIC_MODEL`; install `python -m pip install -e ".[claude]"` |
| OpenAI Responses API | `openai` | `OPENAI_API_KEY`, optional `OPENAI_BASE_URL`, optional `OPENAI_MODEL` |
| Codex HTTP alias | `codex` | same as `openai` |

## Steps

1. **Edit `.env`** and set the provider:

   ```bash
   $EDITOR .env
   ```

   Examples:

   ```dotenv
   LLM_PROVIDER=unleash
   UNLEASH_API_KEY=...
   UNLEASH_ASSISTANT_ID=...

   LLM_PROVIDER=claude
   ANTHROPIC_MODEL=claude-sonnet-4-6

   LLM_PROVIDER=openai
   OPENAI_API_KEY=...
   OPENAI_MODEL=gpt-5.5
   ```

2. **Run a cheap triage smoke:**

   ```bash
   triage-cli triage <ticket-id> --no-logs
   ```

   `--no-logs` confirms the provider call without spending Datadog quota.

3. **If using Claude fallback, verify the local seat:**

   ```bash
   claude --print "ping" --model claude-sonnet-4-6
   ```

## Verification

- The triage command exits `0` and prints a four-section markdown note.
- Provider-specific missing-env errors mention the missing variable before any network request is attempted.
- Output style and length should match expectations for the selected provider/model.

## Troubleshooting

- **`UNLEASH_API_KEY must be set` / `UNLEASH_ASSISTANT_ID must be set`** — fill in Unleash credentials or choose another provider.
- **`OPENAI_API_KEY must be set`** — fill in an OpenAI key for `LLM_PROVIDER=openai` or `LLM_PROVIDER=codex`.
- **`claude-agent-sdk is not installed`** — install the optional Claude fallback extra with `python -m pip install -e ".[claude]"`.
- **"Model not found" or 404** — typo in the provider model ID, or that model is unavailable for the account. Cross-check provider access before changing prompts.
