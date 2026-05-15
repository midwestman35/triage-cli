# ADR 0002 — Prune `claude` and `openai` providers

**Status:** Accepted
**Date:** 2026-05-14

## Context

The v1 reframe shipped with four LLM providers: `unleash` (HTTP to the
internal Axon gateway, default), `claude` (subprocess to the local `claude`
CLI), `openai` (HTTP to the OpenAI Responses API), and `codex` (subprocess to
the local `codex` CLI). Of those, only `unleash` is load-bearing for the
production triage flow; `codex` is the dev escape hatch when the gateway is
unreachable.

`claude` was a workaround left over from an earlier era. As `CLAUDE.md`
already records: *"Do not 'fix' the Claude provider by switching to the
`anthropic` HTTP SDK. The user has an enterprise OAuth seat with no
provisioned Anthropic API key; that path does not work."* The subprocess
provider was the only way to call an Anthropic model at all, and it was
slower than `unleash` and offered no advantage over `codex` for the dev
escape hatch role.

`openai` was kept "for completeness" but never wired into a production path
and never used as a backup. It widened the attack surface (one more API key
in `.env.example`, one more provider for `doctor` to validate) without
buying anything.

## Decision

- Delete the `claude` provider module and all references in `LLM_PROVIDER`
  dispatch, `doctor`, `setup`, and `.env.example`.
- Delete the `openai` provider module and the matching `OPENAI_API_KEY` /
  `OPENAI_MODEL` / `OPENAI_BASE_URL` env vars.
- `LLM_PROVIDER` accepts exactly two values: `unleash` (default) and `codex`.
  Any other value is rejected by `doctor` with a clear error.
- Default codex model is `gpt-5-codex` (env override `CODEX_MODEL`).
- `ANTHROPIC_MODEL` is no longer read by any code path and is removed from
  `.env.example`.

## Consequences

What we gain:

- Smaller release binary (one fewer HTTP client surface plus one fewer
  subprocess wrapper).
- `.env.example` is shorter: no `ANTHROPIC_MODEL`, `OPENAI_API_KEY`,
  `OPENAI_MODEL`, or `OPENAI_BASE_URL`.
- Less surface for accidental misconfiguration (an analyst can no longer set
  `LLM_PROVIDER=claude` and then wonder why `claude --print` is hanging).
- Onboarding is clearer: two providers, both documented, one default.

What we lose:

- No fast escape hatch to a different LLM family if both `unleash` and
  `codex` are down simultaneously. The watcher / inbox stop producing notes
  until at least one of the two is back. Given how rare a simultaneous
  outage of both an internal HTTP gateway and a local CLI subprocess would
  be, this is judged acceptable.

Reversal path:

- The deleted code is recoverable from git history (the prune commit on the
  branch named in this PR). The `LlmProvider` trait in
  `providers/mod.rs` is unchanged, so re-adding a provider is mechanical:
  restore the module file, register it in the dispatch match, add the
  required env vars to `setup.rs` and `doctor`.

## Alternatives considered

- **Keep `claude`.** Rejected. The subprocess was dead code — no documented
  user, no production path, slower than `unleash`. Future Anthropic API
  paths face the same OAuth-only constraint and would need to re-implement
  the subprocess wrapper anyway, at which point recovering it from git is
  trivial.
- **Keep `openai`.** Rejected for the same reasons. The Responses API path
  was never the chosen backup; `codex` covers the "local CLI subprocess"
  role and `unleash` covers the "HTTP to a managed gateway" role.
- **Make providers opt-in via Cargo features.** Rejected. `triage-cli` ships
  as a single static binary that one operator installs; feature flags would
  add build-matrix complexity without any consumer that benefits. Pruning
  in source is the simpler answer for a CLI of this size.

## References

- `docs/decisions/2026-05-14-adversarial-review-of-career-notes.md` (#15)
- `triage-cli-rs/REGRESSIONS.md` (R1, R2)
- The PR this ADR ships in (branch `feat/prune-claude-and-baseline-hardening`)
