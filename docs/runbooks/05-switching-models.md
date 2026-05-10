# Switch LLM providers

> **When to use this:** production should use Unleash, but you need to verify
> provider configuration or temporarily fall back to a local Claude Code seat.

`triage-cli` reads `LLM_PROVIDER` from `.env`. If it is unset, the default is
`unleash`.

## Production: Unleash

```env
LLM_PROVIDER=unleash
UNLEASH_API_KEY=<assistant-scoped API key>
UNLEASH_BASE_URL=https://e-api.unleash.so
UNLEASH_ASSISTANT_ID=<triage assistant ID>
UNLEASH_ACCOUNT=
```

For private tenants or self-hosted Unleash, set `UNLEASH_BASE_URL` to the
tenant's `/e-api` base URL, for example `https://app.acme.unleash.so/e-api`.

`UNLEASH_ACCOUNT` is only needed for impersonated API keys. Leave it blank for
non-impersonated keys.

## Fallback: Claude Code

```env
LLM_PROVIDER=claude
ANTHROPIC_MODEL=claude-sonnet-4-6
```

Install the optional fallback dependency and verify the local OAuth session:

```bash
python -m pip install -e ".[claude]"
claude --print "ping" --model claude-sonnet-4-6
```

## Verification

Use `--no-logs` for a cheap LLM smoke test without Datadog enrichment:

```bash
triage-cli triage <ticket-id> --no-logs --verbose
```

The command should exit `0` and print a structured triage report.

## Troubleshooting

- **Missing Unleash config** — fill `UNLEASH_API_KEY` and
  `UNLEASH_ASSISTANT_ID` in `.env`.
- **Unleash HTTP error** — re-check the API key scope, assistant ID, base URL,
  and optional impersonation account. Include the RequestId in any support
  escalation.
- **Claude fallback import error** — install `.[claude]` in the active venv.
- **Claude auth error** — run `claude` interactively and complete OAuth, then
  retry.
