# Set up triage-cli on a fresh machine

> **When to use this:** brand-new clone of the repo, never run before. Gets you from zero to a working `triage-cli` invocation.

## Steps

Run the interactive setup subcommand from the repo root after building the binary:

```bash
cd triage-cli-rs && cargo build --release && cd ..
./triage-cli-rs/target/release/triage-cli setup
```

The subcommand prompts you for the required env vars, writes `.env`, validates the values, and runs `build-map`. It is idempotent — re-running picks up the last `.env` as defaults.

Use the manual steps below only when you need to diagnose or perform a setup
step yourself.

1. **Verify prerequisites.** The Rust toolchain must exit cleanly:

   ```bash
   cargo --version   # 1.95+
   ```

   If `LLM_PROVIDER=codex`, also verify the codex CLI:

   ```bash
   codex --version
   ```

   If `codex` is missing or unauthenticated, install the codex CLI and run `codex` once interactively to complete OAuth.

2. **Clone the repo (or `cd` into an existing checkout):**

   ```bash
   git clone <repo-url> triage-cli
   cd triage-cli
   ```

3. **Build the release binary:**

   ```bash
   cd triage-cli-rs
   cargo build --release
   cd ..
   ```

   The binary lands at `triage-cli-rs/target/release/triage-cli`. Optionally symlink it onto `PATH`:

   ```bash
   ln -s "$PWD/triage-cli-rs/target/release/triage-cli" ~/.local/bin/triage-cli
   ```

4. **Configure `.env`:**

   ```bash
   cp .env.example .env
   ```

   Fill in the following keys (see `README.md` for the full table):

   - `ZENDESK_SUBDOMAIN`, `ZENDESK_EMAIL`, `ZENDESK_API_TOKEN` — generate the API token in Zendesk Admin Center under Apps and integrations -> Zendesk API. Do **not** append `/token` to the email; the client does that.
   - `LLM_PROVIDER` — `unleash` (default, HTTP to the internal Axon gateway) or `codex` (subprocess to the local `codex` CLI). These are the only accepted values as of 2026-05-14; see `docs/adr/0002-prune-claude-openai-providers.md`.
     - For `unleash`: set `UNLEASH_API_KEY` and `UNLEASH_ASSISTANT_ID`. The model is chosen server-side by the assistant.
     - For `codex`: ensure the `codex` CLI is on `PATH`. Optionally set `CODEX_MODEL` (default `gpt-5-codex`).
   - `DD_API_KEY`, `DD_APP_KEY` — optional. Add these only if you plan to use Datadog enrichment in `triage`, `watch`, or `inbox`; Guided Investigation does not need them.

5. **Build the site map** if you will use one-shot triage or watcher site resolution (turns `apex-cnc-inventory.md` into `data/cnc-map.json`):

   ```bash
   triage-cli build-map
   ```

6. **Run `doctor` to validate the environment:**

   ```bash
   triage-cli doctor
   ```

   Exits 0 when all critical checks pass (Zendesk creds, selected provider credential, output directory writable). Datadog is a warning only.

7. **Smoke-test Guided Investigation against a ticket you are assigned.** Pick a ticket ID from your own Zendesk queue and run:

   ```bash
   triage-cli investigate <ticket-id>
   ```

   This exercises Zendesk auth, the configured LLM provider, and the ticket-folder writer end to end. Do not run this against a shared view or someone else's queue — see `docs/runbooks/08-read-only-my-queue-flow.md` for the conservative certification flow.

## Verification

- `triage-cli --help` lists `investigate`, `triage`, `inbox`, `watch`, `doctor`, `build-map`, and `setup` subcommands.
- `data/cnc-map.json` has at least 30 entries:

  ```bash
  python3 -c "import json; print(len(json.load(open('data/cnc-map.json'))))"
  ```

  (Plain `jq 'length' data/cnc-map.json` works too if `jq` is installed.)

- `triage-cli doctor` exits 0.
- A successful `triage-cli investigate <ticket-id>` writes `Tickets/<id>/{INTAKE,EVIDENCE_PREFLIGHT,FORK_PACKET,DRAFTS,STATE}.md`.

## Troubleshooting

- **`command not found: triage-cli`** — the binary is not on `PATH`. Either invoke it as `./triage-cli-rs/target/release/triage-cli ...` or create the symlink shown in step 3.
- **`✗ <PROVIDER_KEY> not set`** from `doctor` — the selected `LLM_PROVIDER` does not have its required credential. Either set the env var or switch provider via `LLM_PROVIDER=...`.
- **`✗ codex not on PATH`** from `doctor` — `LLM_PROVIDER=codex` requires the `codex` CLI. Install it and ensure `which codex` succeeds.
- **Zendesk auth failed (401/403)** — the email already has `/token` appended in `.env` (remove it), or the API token was pasted with whitespace, or the token does not have ticket read scope.
