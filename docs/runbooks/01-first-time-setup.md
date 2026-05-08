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
   python -m ensurepip --upgrade
   python -m pip install --upgrade pip setuptools wheel
   python -m pip install -e ".[dev]"
   ```

   This pulls in runtime deps plus `pytest` and `ruff`. Use `python -m pip` from
   inside the activated venv so the install targets the same interpreter even if
   a bare `pip` command is not on `PATH`.

5. **Configure `.env`:**

   ```bash
   cp .env.example .env
   ```

   Fill in the following keys (see `README.md` for the full table):

   - `ZENDESK_SUBDOMAIN`, `ZENDESK_EMAIL`, `ZENDESK_API_TOKEN` — generate the API token in Zendesk Admin Center under Apps and integrations -> Zendesk API. Do **not** append `/token` to the email; the client does that.
   - `DD_API_KEY`, `DD_APP_KEY` — optional. Add these only if you plan to use Datadog enrichment in `triage`, `watch`, or `inbox`; Guided Investigation does not need them.

6. **Build the site map** if you will use one-shot triage or watcher site resolution (turns `apex-cnc-inventory.md` into `data/cnc-map.json`):

   ```bash
   triage-cli build-map
   ```

7. **Run read-only Guided Investigation verification only against your assigned Zendesk queue.**

   Follow `docs/runbooks/08-read-only-my-queue-flow.md` exactly: discover ticket IDs with `ZendeskClient.list_my_ticket_ids()`, select one returned assigned ticket, run Guided Investigation with that ID, and do not use `--save` or any Zendesk write action. This exercises Zendesk auth and local markdown draft rendering without Datadog, CNC/site resolution, or Claude.

## Verification

- `triage-cli --help` lists `investigate`, `triage`, `watch`, and `build-map` subcommands.
- `data/cnc-map.json` has at least 30 entries:

  ```bash
  python -c "import json; print(len(json.load(open('data/cnc-map.json'))))"
  ```

- The assigned-queue-only verification in step 7 prints a local Guided Investigation markdown draft.
- The final certification runbook confirms the full read-only assigned-queue flow.

## Troubleshooting

- **`zsh: command not found: pip`** — the venv does not expose a bare `pip` shim. Run `python -m ensurepip --upgrade`, then use `python -m pip install -e ".[dev]"`.
- **`command not found: triage-cli`** — venv isn't active, or `python -m pip install -e .` didn't run. Re-activate (`source .venv/bin/activate`) and re-install.
- **`ImportError: claude_agent_sdk`** — the SDK didn't install. Run `python -m pip install claude-agent-sdk` and confirm the `claude` CLI itself is installed (`claude --version`).
- **Zendesk auth failed (401/403)** — the email already has `/token` appended in `.env` (remove it), or the API token was pasted with whitespace, or the token doesn't have ticket read scope.
