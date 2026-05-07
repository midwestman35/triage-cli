# Set up triage-cli on a fresh machine

> **When to use this:** brand-new clone of the repo, never run before. Gets you from zero to a working `triage-cli` invocation.

## Steps

1. **Verify prerequisites.** Both commands must exit cleanly:

   ```bash
   claude --version
   python3.11 --version
   ```

   If `claude` is missing, install Claude Code first and run `claude` once interactively to complete OAuth. The Agent SDK piggybacks on that session — there is no separate API key.

2. **Clone the repo (or `cd` into an existing checkout):**

   ```bash
   git clone <repo-url> triage-cli
   cd triage-cli
   ```

3. **Create and activate a virtualenv pinned to 3.11:**

   ```bash
   python3.11 -m venv .venv
   source .venv/bin/activate
   ```

4. **Install the package in editable mode with dev extras:**

   ```bash
   pip install -e ".[dev]"
   ```

   This pulls in runtime deps plus `pytest` and `ruff`.

5. **Configure `.env`:**

   ```bash
   cp .env.example .env
   ```

   Fill in the following keys (see `README.md` for the full table):

   - `ZENDESK_SUBDOMAIN`, `ZENDESK_EMAIL`, `ZENDESK_API_TOKEN` — generate the API token in Zendesk Admin Center under Apps and integrations -> Zendesk API. Do **not** append `/token` to the email; the client does that.
   - `DD_API_KEY`, `DD_APP_KEY` — generate at <https://app.datadoghq.com/organization-settings/api-keys> and the Application Keys tab on the same page. Both keys are required and they are different.

6. **Build the site map** (turns `apex-cnc-inventory.md` into `data/cnc-map.json`):

   ```bash
   triage-cli build-map
   ```

7. **Smoke test against a real ticket** without spending Datadog quota:

   ```bash
   triage-cli triage <ticket-id> --verbose --no-logs
   ```

   This exercises Zendesk auth, the site lookup, and the LLM call but skips Datadog.

## Verification

- `triage-cli --help` lists both `triage` and `build-map` subcommands.
- `data/cnc-map.json` has at least 30 entries:

  ```bash
  python -c "import json; print(len(json.load(open('data/cnc-map.json'))))"
  ```

- The smoke-test command from step 7 prints a four-section markdown note (Summary / Log signals / Likely cause / Suggested first action).

## Troubleshooting

- **`command not found: triage-cli`** — venv isn't active, or `pip install -e .` didn't run. Re-activate (`source .venv/bin/activate`) and re-install.
- **`ImportError: claude_agent_sdk`** — the SDK didn't install. Run `pip install claude-agent-sdk` and confirm the `claude` CLI itself is installed (`claude --version`).
- **Zendesk auth failed (401/403)** — the email already has `/token` appended in `.env` (remove it), or the API token was pasted with whitespace, or the token doesn't have ticket read scope.
