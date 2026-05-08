# Investigate or triage a Zendesk ticket

> **When to use this:** a ticket lands in your queue and you want a structured first read, a local handoff draft, or a fast one-shot summary.

Use `investigate` first when you are gathering evidence. Use `triage` when you want the existing fast report path with optional site/Datadog/Claude enrichment.

## Steps

1. **Grab the ticket ID or full URL** from Zendesk. Either form works:

   ```
   12345
   https://<sub>.zendesk.com/agent/tickets/12345
   ```

2. **Run Guided Investigation:**

   ```bash
   triage-cli investigate 12345
   ```

   Or with the URL:

   ```bash
   triage-cli investigate https://<sub>.zendesk.com/agent/tickets/12345
   ```

   This fetches the Zendesk ticket, comments, and attachment metadata, then prints a local markdown handoff draft. It does not need Datadog credentials, CNC/site resolution, or Claude auth, and it does not post to Zendesk.

3. **Add evidence when you already have it:**

   ```bash
   # Add local logs.
   triage-cli investigate 12345 --file ./station.log

   # Add pasted console/log evidence.
   triage-cli investigate 12345 --paste "console=WARN audio dropped"

   # Save paired markdown/JSON artifacts under ./triage-notes/.
   triage-cli investigate 12345 --file ./station.log --paste "console=WARN audio" --save
   ```

   `--file` and `--paste LABEL=TEXT` may be repeated.

4. **Use one-shot triage when you want the enriched fast report:**

   ```bash
   triage-cli triage 12345
   ```

   Or with the URL:

   ```bash
   triage-cli triage https://<sub>.zendesk.com/agent/tickets/12345
   ```

5. **Read the markdown note** that prints to stdout.

   Guided Investigation reports the evidence reviewed, current likely cause assessment, unknowns/gaps, next steps, and a suggested internal note.

   One-shot triage keeps the older four-section shape:
   - `## Summary` — what the ticket reports, no speculation
   - `## Log signals` — what the Datadog window shows
   - `## Likely cause (inference)` — best guess, marked as inference
   - `## Suggested first action` — one concrete next step

6. **One-shot flags worth knowing.** Layer these on as the situation calls for it:

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

   # Skip Datadog entirely for one-shot triage.
   triage-cli triage 12345 --no-logs
   ```

   Stack flags as needed, e.g. `--verbose --save --at <iso>`.

## Verification

- A markdown note prints to stdout.
- Exit code is `0`:

  ```bash
  triage-cli investigate 12345; echo "exit=$?"
  ```

- With `--save`: `ls triage-notes/` shows files named `<ticket-id>-<timestamp>.md` and `<ticket-id>-<timestamp>.json`.

## Troubleshooting

- **`--paste must be LABEL=TEXT`** — pass pasted evidence with a short label before the equals sign, for example `--paste "console=WARN audio dropped"`.
- **`Local evidence file not found`** — the `--file` path must point to an existing file on your machine.
- **"could not resolve site for ticket"** — one-shot `triage` could not match requester org or ticket text in `data/cnc-map.json`. Pass `--site <site_name>` or `--cnc <uuid>` to bypass lookup, or use `investigate` when site/Datadog enrichment is not needed.
- **Empty Log signals section** — for one-shot `triage`, re-run with `--verbose` to see the resolved anchor and log count. If the anchor is `created_at` but the incident happened earlier, override with `--at <iso8601>`. If the window is too narrow, add `--window-minutes 60`.
- **"Claude Agent SDK call failed"** — one-shot `triage` or watcher mode could not use your Claude Code OAuth session. Re-auth interactively:

  ```bash
  claude /login
  ```

  Then re-run the triage command.
