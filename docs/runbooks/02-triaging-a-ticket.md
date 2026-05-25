# Investigate or triage a Zendesk ticket

> **When to use this:** a ticket lands in your queue and you want a structured
> first read, a local ticket folder, or a fast one-shot handoff.

Use `investigate` first when you are gathering evidence interactively. Use
`triage` when you want the headless path with the same structured pipeline.

## Steps

1. **Grab the ticket ID or full URL** from Zendesk. Either form works:

   ```text
   12345
   https://<sub>.zendesk.com/agent/tickets/12345
   ```

2. **Run Guided Investigation:**

   ```bash
   triage-cli investigate 12345
   triage-cli investigate https://<sub>.zendesk.com/agent/tickets/12345
   ```

   This fetches the Zendesk ticket, comments, and attachment metadata, then
   runs the structured assessment. It writes
   `${TRIAGE_TICKETS_ROOT:-./Tickets}/<id>/` and prints `FORK_PACKET.md` to
   stdout. It does not post to Zendesk.

3. **Add evidence when you already have it:**

   ```bash
   triage-cli investigate 12345 --file ./station.log
   triage-cli investigate 12345 --paste "console=WARN audio dropped"
   triage-cli investigate 12345 --file ./station.log --paste "console=WARN audio"
   ```

   `--file` and `--paste LABEL=TEXT` may be repeated.

4. **Use one-shot triage when you want the headless report:**

   ```bash
   triage-cli triage 12345
   triage-cli triage https://<sub>.zendesk.com/agent/tickets/12345
   ```

5. **Replay a fixture offline when you are testing the pipeline itself:**

   ```bash
   triage-cli investigate --fixture triage-cli-rs/fixtures/audio-drop --no-llm --force
   triage-cli triage 55001 --fixture triage-cli-rs/fixtures/audio-drop --no-llm --force
   ```

   `investigate --fixture` uses the fixture's `ticket.json` instead of
   fetching live Zendesk data, so it skips attachment-download prompts. Fixture
   and demo mode also load canned Datadog and memory inputs, so they do not
   require Zendesk creds, Datadog creds, or a prebuilt
   `$TRIAGE_HOME/data/cnc-map.json`.

6. **Read the output and ticket folder.**

   - stdout prints `FORK_PACKET.md`, the pipeable handoff surface.
   - `Tickets/<id>/INTAKE.md` captures engine-known ticket facts.
   - `Tickets/<id>/EVIDENCE_PREFLIGHT.md` lists gathered and missing evidence.
   - `Tickets/<id>/DRAFTS.md` contains CONFIRM-gated drafts.
   - `Tickets/<id>/STATE.md` records fork, confidence, owner, status, and
     rubric metadata for inbox and soft-locks.

7. **Flags worth knowing.** Layer these on as the situation calls for it:

   ```bash
   triage-cli triage 12345 --verbose
   triage-cli triage 12345 --at "2026-05-07T14:32:00Z"
   triage-cli triage 12345 --site us-foo-apex-bar
   triage-cli triage 12345 --cnc 921d7c53-e815-...
   triage-cli triage 12345 --levels error,warn,info
   triage-cli triage 12345 --no-logs
   triage-cli triage 12345 --no-llm
   triage-cli triage 12345 --force
   triage-cli triage 12345 --diff
   ```

   Stack flags as needed, for example `--verbose --at <iso>`.

## Verification

- `FORK_PACKET.md` prints to stdout.
- Exit code is `0`:

  ```bash
  triage-cli investigate 12345; echo "exit=$?"
  ```

- `Tickets/<id>/` contains `INTAKE.md`, `EVIDENCE_PREFLIGHT.md`,
  `FORK_PACKET.md`, `DRAFTS.md`, and `STATE.md`.

## Troubleshooting

- **`--paste must be LABEL=TEXT`** - pass pasted evidence with a short label
  before the equals sign, for example `--paste "console=WARN audio dropped"`.
- **`Local evidence file not found`** - the `--file` path must point to an
  existing file on your machine.
- **"could not resolve site for ticket"** - pass `--site <site_name>` or
  `--cnc <uuid>` to bypass lookup, or update `apex-cnc-inventory.md` and rerun
  `build-map`.
- **Empty log evidence** - re-run with `--verbose` to see the resolved anchor
  and log count. If the anchor is `created_at` but the incident happened
  earlier, override with `--at <iso8601>`. If the window is too narrow, add
  `--window-minutes 60`.
- **LLM provider error** - `doctor` and the runtime check both flag missing
  provider credentials. Set env vars for `LLM_PROVIDER`, or switch provider
  (`unleash` / `codex`) via `.env`. See `docs/runbooks/05-switching-models.md`.
