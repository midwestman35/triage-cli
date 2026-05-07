# Switch the Claude model

> **When to use this:** you want to try a different Claude model (cost, latency, or capability tradeoff), or pin to an older version for reproducibility.

The model identifier is read from `ANTHROPIC_MODEL` in `.env` and passed to `ClaudeAgentOptions(model=...)` in `triage_cli/llm.py`. There is no provider abstraction — only Claude models work.

## Steps

1. **Edit `.env`** and change `ANTHROPIC_MODEL` to the desired model ID:

   ```bash
   $EDITOR .env
   ```

   Examples:

   ```
   ANTHROPIC_MODEL=claude-sonnet-4-6
   ANTHROPIC_MODEL=claude-opus-4-7
   ANTHROPIC_MODEL=claude-haiku-4-5-20251001
   ```

2. **(Optional) Verify the model is available** on your Claude Code seat:

   ```bash
   claude --print "ping" --model claude-opus-4-7
   ```

   If this returns a response, the model is callable. If it errors with "model not found" or similar, your seat doesn't have access — pick a different ID.

3. **Run a triage with the new model:**

   ```bash
   triage-cli triage <ticket-id> --no-logs
   ```

   `--no-logs` is a cheap way to confirm the LLM call succeeds without burning Datadog quota.

## Verification

- The triage command exits `0` and prints a four-section markdown note.
- Output style and length should match expectations for the model family. If you switched from Sonnet to Haiku, expect terser output; from Sonnet to Opus, expect more deliberate reasoning in the inference section.
- `--verbose` does **not** currently log the resolved model name. If you want to confirm at runtime, temporarily edit `triage_cli/llm.py` to log `MODEL` at module import or inside `triage()`. Otherwise, trust the env var.

## Troubleshooting

- **"Model not found" or 404 from the SDK** — typo in the model ID, or the model isn't available on your Claude Code seat. Cross-check with `claude --print "ping" --model <id>`.
- **Triage note structure looks wildly different** — expected when switching across model families (e.g. Sonnet -> Haiku). The system prompt fixes the four section headers, but tone and depth vary by model. The project's tested model is `claude-sonnet-4-6`; output on other models is best-effort.
- **`.env` change not picked up** — `python-dotenv` is loaded at CLI import. Make sure you're not pointing at a stale `.env` (e.g. if you have one in `~/` and one in the repo, the repo's wins because that's `cwd`). Confirm with:

  ```bash
  grep ANTHROPIC_MODEL .env
  ```

> **Note:** the project's tested model is `claude-sonnet-4-6`. Other models will run, but the four-section markdown output may vary slightly in structure or tone.
