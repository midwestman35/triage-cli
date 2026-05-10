# Common errors and fixes

> **When to use this:** something failed and you need to find the fix fast. Errors are grouped by symptom.

## Auth failures

### Zendesk 401 / 403

```
RuntimeError: Zendesk auth failed (401)
```

- The client appends `/token` to the email when forming Basic auth. If your `.env` already has `/token` on `ZENDESK_EMAIL`, remove it.
- The token may have been pasted with leading/trailing whitespace. Re-paste cleanly into `.env`.
- If the token is correct but auth still fails, regenerate it in Zendesk Admin Center -> Apps and integrations -> Zendesk API -> API tokens, and confirm the agent has read scope on tickets.

### Datadog 401 / 403

```
ApiException: 401 Unauthorized
```

- `DD_API_KEY` and `DD_APP_KEY` are **separate keys**. Both are required, and they're not interchangeable. Confirm both are set in `.env` and not swapped.
- The APP key must belong to a user with `logs_read_data` permission.
- If you're on a non-US Datadog tenant, set `DD_SITE` (e.g. `datadoghq.eu`).

### Claude Agent SDK auth

```
Claude Agent SDK call failed: ...
```

- The SDK inherits Claude Code's OAuth session. Run `claude` once interactively in the same shell to verify auth state. If it prompts you to log in, complete that and re-run the triage command.
- The SDK does **not** read `ANTHROPIC_API_KEY`. Setting one will not help.

## Site resolution

### "could not resolve site for ticket"

The requester org didn't exact-match a `friendly_name`, and no `site_name` or `friendly_name` substring appeared in the subject/body.

- Bypass with `--site <site_name>` (used directly in the Datadog filter) or `--cnc <uuid>` (looked up in `data/cnc-map.json`).
- If the customer is missing from the map, refresh the inventory (see `03-refreshing-the-site-map.md`), then re-run `triage-cli build-map`.

### Wrong customer matched

The ticket subject contained a substring that matched a different customer's `friendly_name` or `site_name`.

- Pass `--site <site_name>` explicitly to override the lookup.
- Run with `--verbose` to see which strategy hit (it's logged as `Site resolved via <strategy>: ...`).

## Datadog query

### "site_name '...' contains characters that are unsafe"

Validation in `triage_cli/datadog.py` rejected the resolved `site_name` before it hit the query string.

- Bug in the map. Fix the offending row in `apex-cnc-inventory.md` (lowercase-with-hyphens, no spaces or special chars), then re-run `triage-cli build-map`.
- One-off bypass: pass a clean value via `--site us-foo-apex-bar`.

### Empty Log signals despite expected logs

- Run with `--verbose` and check the printed anchor and window:

  ```bash
  triage-cli triage <id> --verbose 2>&1 | grep -E "Anchor|window|Pulled"
  ```

- If the anchor source is `created_at` but the incident was hours earlier, override with `--at "2026-05-07T14:32:00Z"`.
- If the window is too narrow, widen it with `--window-minutes 60` (or larger).
- Confirm the levels filter isn't excluding the lines you want — default is `error,warn`. Add `info` with `--levels error,warn,info`.

### Datadog 429 (rate limited)

Wait the duration suggested in the error and re-run. There are no automatic retries in v1.

## LLM

### Empty triage note

The LLM returned no assistant text blocks. Almost always an SDK auth issue — re-run `claude` interactively to refresh the session, then retry.

### Anchor extraction returns null in `--verbose`, but the ticket text has a timestamp

The anchor-extraction prompt instructs the model to return null when the timestamp is ambiguous. If the ticket's wording is loose ("this morning around 9"), the model will usually return null on purpose. Force the anchor with `--at <iso8601>` instead of fighting the prompt.

## Files / paths

### `data/cnc-map.json` not found

```
FileNotFoundError: data/cnc-map.json
```

The map hasn't been built yet, or you're running from the wrong cwd. Run from the repo root:

```bash
triage-cli build-map
```

### `--save` puts the note somewhere unexpected

`./triage-notes/` is **cwd-relative**, not repo-relative. If you ran the command from `~/Desktop`, the note is at `~/Desktop/triage-notes/`. `cd` into the repo first if you want it under the repo tree.

## Redactor

If a triage note references `<PHONE>` or `<ADDR>` where the original ticket had operational data (e.g., a long station ID matched the phone regex), re-run with `--no-redact` to confirm. Open an issue with the offending input so the pattern can be tightened.
