# Certify the read-only assigned-queue flow

> **When to use this:** final verification for Guided Investigation after code changes, using only the authenticated agent's assigned Zendesk queue. This runbook is intentionally read-only and must be run only against your own Zendesk account and queue.

## Guardrails

- Do not run this against a shared view, arbitrary view ID, broad search, or copied ticket ID from outside the authenticated user's assigned queue.
- Do not run any Zendesk write action. This runbook must not post, update, delete, comment, assign, tag, or otherwise mutate Zendesk data.
- Do not pass `--save` during certification. The expected artifact is stdout only, with verbose progress on stderr.
- `triage-cli investigate` fetches Zendesk ticket data, comments, and attachment metadata, then prints a local markdown draft. It does not post/update/delete/comment in Zendesk.
- Datadog, CNC/site resolution, and LLM access are not required for `investigate`.

## Steps

1. **Start from the repo root and verify local checks.**

   ```bash
   .venv/bin/pytest
   .venv/bin/ruff check .
   git diff --check
   test ! -e uv.lock
   ```

   All four commands must exit `0`. Do not run `uv run` for this certification.

2. **Recommended: run the automated assigned-queue certification runner.**

   The script below automates the read-only Zendesk environment check, assigned-queue
   discovery, ticket selection, ticket fetch, Guided Investigation draft rendering, and
   evidence count/status output. It does not use view APIs, search APIs, Datadog,
   CNC/site resolution, LLM access, Zendesk writes, or saved output files.

   ```bash
   .venv/bin/python scripts/certify_readonly_my_queue.py
   ```

   By default, the script selects the first ticket returned by
   `ZendeskClient.list_my_ticket_ids()`. To certify a specific ticket, pass an ID that is
   present in that same assigned queue:

   ```bash
   .venv/bin/python scripts/certify_readonly_my_queue.py --ticket-id "$ticket_id"
   ```

   Optional local/pasted evidence mirrors the safe subset of `triage-cli investigate`:

   ```bash
   .venv/bin/python scripts/certify_readonly_my_queue.py \
     --ticket-id "$ticket_id" \
     --file "$tmp_evidence" \
     --paste "certification=local pasted evidence only"
   ```

   Expected outcome:

   - stdout contains the local markdown investigation draft.
   - stderr reports required Zendesk variables as `set` or `missing`, assigned queue
     count, selected ticket ID, fetched ticket, evidence counts, and sources.
   - if a required environment variable is missing, the script exits nonzero before
     instantiating `ZendeskClient`.
   - if `--ticket-id` is provided but is not in the authenticated user's assigned queue,
     the script exits nonzero before fetching the ticket.
   - malformed `--paste` values and missing/non-file/unreadable `--file` paths are
     rejected before any Zendesk fetch.
   - no files are written under `triage-notes/`; there is no `--save` option.

3. **Manual fallback/audit step: load local environment if needed.**

   If your shell does not already have Zendesk credentials exported, load `.env` without printing it:

   ```bash
   set -a
   source .env
   set +a
   ```

4. **Manually verify required Zendesk variables are present without printing secret values.**

   ```bash
   .venv/bin/python - <<'PY'
   import os
   import sys

   required = ("ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN")
   missing = [name for name in required if not os.environ.get(name)]
   for name in required:
       print(f"{name}: {'set' if os.environ.get(name) else 'missing'}")
   if missing:
       sys.exit(f"Missing required Zendesk environment variables: {', '.join(missing)}")
   PY
   ```

   This confirms presence only; it must not echo the subdomain, email, or token values.

5. **Manually query only your authenticated assigned queue.**

   Use the client method that fetches `/users/me.json` and then `/users/{id}/tickets/assigned.json`:

   ```bash
   .venv/bin/python - <<'PY'
   from triage_cli.zendesk import ZendeskClient

   with ZendeskClient.from_env() as zd:
       ticket_ids = zd.list_my_ticket_ids()

   print(f"assigned_ticket_count={len(ticket_ids)}")
   print("ticket_ids=" + " ".join(str(ticket_id) for ticket_id in ticket_ids[:25]))
   PY
   ```

   This is the only allowed live ticket-discovery step. Do not use `list_view_ticket_ids()`, Zendesk view IDs, search APIs, broad URL guessing, or any ticket not returned by `ZendeskClient.list_my_ticket_ids()`.

6. **Manually select one ticket ID from that returned list.**

   ```bash
   ticket_id=<one-id-from-ticket_ids>
   ```

   The selected ID must be present in the `ticket_ids=` output from step 5.

7. **Manually run Guided Investigation against that assigned queue ticket only.**

   ```bash
   triage-cli investigate "$ticket_id" --verbose
   ```

   Expected outcome:

   - stdout contains the local markdown investigation draft.
   - stderr includes `Fetched ticket #...` and an `Investigation evidence:` line with comment, attachment metadata, local file, pasted evidence, and source counts.
   - no Datadog credentials, CNC map, site lookup, or LLM access are required.
   - no Zendesk note, post, update, delete, comment, assignment, or tag change occurs.

8. **Optionally test local/pasted evidence without saving.**

   This still uses the same selected assigned-queue ticket and still must not use `--save`:

   ```bash
   tmp_evidence="$(mktemp)"
   trap 'rm -f "$tmp_evidence"' EXIT
   printf 'local certification evidence only\n' > "$tmp_evidence"

   triage-cli investigate "$ticket_id" \
     --file "$tmp_evidence" \
     --paste "certification=local pasted evidence only" \
     --verbose
   ```

   Expected outcome:

   - stdout contains the local markdown draft with the added local/pasted evidence reflected in the evidence summary.
   - stderr shows the same fetched ticket line and evidence counts, with local files and pasted evidence greater than zero.
   - no files are written under `triage-notes/` because `--save` was not used.
   - no Zendesk write action occurs.

## Troubleshooting

- **Missing `ZENDESK_SUBDOMAIN`, `ZENDESK_EMAIL`, or `ZENDESK_API_TOKEN`** — load `.env` with `set -a; source .env; set +a`, or export the three variables in the shell. Re-run the presence check; do not paste secrets into logs.
- **Assigned queue is empty** — stop certification. Ask the queue owner to assign a safe test ticket to the authenticated user, then repeat the `list_my_ticket_ids()` step. Do not substitute a view ID, broad search, or unrelated ticket ID.
- **Zendesk auth failed (401/403)** — confirm the email does not already include `/token`, the token has not expired or been pasted with whitespace, and the account has read access to assigned tickets.
- **Zendesk fetch failed or ticket not found** — confirm the ticket ID was copied exactly from the `list_my_ticket_ids()` output in this run. Re-run the assigned-queue query and select a currently returned ID.
- **No markdown draft prints** — re-run with `--verbose` and inspect stderr. The expected failure modes are local environment, Zendesk auth, or Zendesk fetch errors; Datadog, CNC, and Claude should not be involved in `investigate`.
