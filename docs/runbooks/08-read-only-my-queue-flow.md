# Certify the read-only assigned-queue flow

> **When to use this:** final verification for Guided Investigation after code
> changes, using only the authenticated agent's assigned Zendesk queue. This
> runbook is intentionally read-only with respect to Zendesk and must be run
> only against your own Zendesk account and queue.

## Guardrails

- Do not run this against a shared view, arbitrary view ID, broad search, or
  copied ticket ID from outside the authenticated user's assigned queue.
- Do not run any Zendesk write action. This runbook must not post, update,
  delete, comment, assign, tag, or otherwise mutate Zendesk data.
- `triage-cli investigate` always writes a local ticket folder and prints
  `FORK_PACKET.md` to stdout. Treat the ticket folder as a local artifact only;
  this runbook still must not mutate Zendesk.
- Datadog enrichment and CNC/site resolution are not required for the assigned
  queue certification path.

## Steps

1. **Start from the Rust crate and verify local checks.**

   ```bash
   cd triage-cli-rs
   cargo test
   cargo clippy --all-targets -- -D warnings
   cargo fmt --all -- --check
   cd ..
   git diff --check
   ```

   All commands must exit `0`.

2. **Load local environment if needed.**

   If your shell does not already have Zendesk credentials exported, load `.env`
   without printing it:

   ```bash
   set -a
   source .env
   set +a
   ```

3. **Verify required Zendesk variables are present without printing secrets.**

   ```bash
   python - <<'PY'
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

   This confirms presence only; it must not echo the subdomain, email, or token
   values.

4. **Query only your authenticated assigned queue.**

   Use the CLI path or a small local helper that fetches `/users/me.json` and
   then `/users/{id}/tickets/assigned.json`. Do not use view APIs, search APIs,
   broad URL guessing, or any ticket not returned by the authenticated assigned
   queue.

5. **Select one ticket ID from that returned list.**

   ```bash
   ticket_id=<one-id-from-your-assigned-queue>
   ```

   The selected ID must be present in the assigned-queue output from this run.

6. **Run Guided Investigation against that assigned queue ticket only.**

   ```bash
   triage-cli investigate "$ticket_id" --verbose
   ```

   Expected outcome:

   - stdout contains `FORK_PACKET.md`.
   - stderr includes progress/status lines and the final ticket-folder path.
   - `${TRIAGE_TICKETS_ROOT:-./Tickets}/<id>/` contains `INTAKE.md`,
     `EVIDENCE_PREFLIGHT.md`, `FORK_PACKET.md`, `DRAFTS.md`, and `STATE.md`.
   - no Zendesk note, post, update, delete, comment, assignment, or tag change
     occurs.

7. **Optionally test local/pasted evidence.**

   This still uses the same selected assigned-queue ticket:

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

   - stdout contains `FORK_PACKET.md` with the added local/pasted evidence
     reflected in the evidence summary.
   - stderr shows the same fetched ticket line and evidence counts, with local
     files and pasted evidence greater than zero.
   - the local ticket folder is written under
     `${TRIAGE_TICKETS_ROOT:-./Tickets}/<id>/`.
   - no Zendesk write action occurs.

## Troubleshooting

- **Missing `ZENDESK_SUBDOMAIN`, `ZENDESK_EMAIL`, or `ZENDESK_API_TOKEN`** -
  load `.env` with `set -a; source .env; set +a`, or export the three variables
  in the shell. Re-run the presence check; do not paste secrets into logs.
- **Assigned queue is empty** - stop certification. Ask the queue owner to
  assign a safe test ticket to the authenticated user, then repeat assigned
  queue discovery. Do not substitute a view ID, broad search, or unrelated
  ticket ID.
- **Zendesk auth failed (401/403)** - confirm the email does not already include
  `/token`, the token has not expired or been pasted with whitespace, and the
  account has read access to assigned tickets.
- **Zendesk fetch failed or ticket not found** - confirm the ticket ID was copied
  exactly from the assigned-queue output in this run.
- **No `FORK_PACKET.md` prints** - re-run with `--verbose` and inspect stderr.
  The expected failure modes are local environment, Zendesk auth, Zendesk fetch,
  or provider errors.
