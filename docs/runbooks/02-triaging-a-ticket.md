# Triage a Zendesk ticket

> **When to use this:** a ticket lands in your queue and you want a structured first read before digging in by hand.

## Steps

1. **Grab the ticket ID or full URL** from Zendesk. Either form works:

   ```
   12345
   https://<sub>.zendesk.com/agent/tickets/12345
   ```

2. **Run the basic command:**

   ```bash
   triage-cli triage 12345
   ```

   Or with the URL:

   ```bash
   triage-cli triage https://<sub>.zendesk.com/agent/tickets/12345
   ```

3. **Read the four-section markdown note** that prints to stdout:
   - `## Summary` — what the ticket reports, no speculation
   - `## Log signals` — what the Datadog window shows
   - `## Likely cause (inference)` — best guess, marked as inference
   - `## Suggested first action` — one concrete next step

4. **Flags worth knowing.** Layer these on as the situation calls for it:

   ```bash
   # Show pipeline progress (site strategy, anchor source, log count) on stderr.
   triage-cli triage 12345 --verbose

   # Also write the note to ./triage-notes/<id>-<timestamp>.md.
   triage-cli triage 12345 --save

   # Override the anchor when the customer reported the incident hours late.
   triage-cli triage 12345 --at "2026-05-07T14:32:00Z"

   # Force the site when the ticket text doesn't name the customer cleanly.
   triage-cli triage 12345 --site us-foo-apex-bar

   # Widen the log filter beyond the default error,warn.
   triage-cli triage 12345 --levels error,warn,info

   # Skip Datadog entirely (faster, useful when iterating on prompts).
   triage-cli triage 12345 --no-logs
   ```

   Stack flags as needed, e.g. `--verbose --save --at <iso>`.

## Verification

- The four-section markdown note prints to stdout.
- Exit code is `0`:

  ```bash
  triage-cli triage 12345; echo "exit=$?"
  ```

- With `--save`: `ls triage-notes/` shows a file named `<ticket-id>-<timestamp>.md`.

## Troubleshooting

- **"could not resolve site for ticket"** — the requester org and ticket text didn't match anything in `data/cnc-map.json`. Pass `--site <site_name>` or `--cnc <uuid>` to bypass lookup, or drop `--no-interactive` so the CLI prompts you.
- **Empty Log signals section** — re-run with `--verbose` to see the resolved anchor and log count. If the anchor is `created_at` but the incident happened earlier, override with `--at <iso8601>`. If the window is too narrow, add `--window-minutes 60`.
- **"Claude Agent SDK call failed"** — your Claude Code OAuth session expired. Re-auth interactively:

  ```bash
  claude /login
  ```

  Then re-run the triage command.
