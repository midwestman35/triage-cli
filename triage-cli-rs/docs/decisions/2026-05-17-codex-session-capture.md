# Codex Session-ID Capture Method (2026-05-17)

**Status:** Decided per ADR conventions; supersedes the spec's open question
(`docs/superpowers/specs/2026-05-17-interactive-investigation-design.md` § 5.6
and § 12.1).

**Discharges:** Task 1 of the interactive-investigation v1 implementation
plan (the codex contract gate that blocks all subsequent codex `followup`
work).

**Evidence source:** `triage-cli-rs/tests/codex_contract.rs`, run on
`codex-cli 0.130.0` against ChatGPT auth (`auth_mode = "chatgpt"`, model
`gpt-5.5`).

## Method selected

### A. `codex exec --json` carries the session ID

The first JSONL record emitted on stdout by `codex exec --json` is a
`thread.started` event that carries the session/thread identifier:

```json
{"type":"thread.started","thread_id":"019e373e-ef1d-7f42-a013-497144960d5c"}
```

**Capture method label:** `codex_json_output`.

**Parsing rule:** Read stdout line-by-line; for each line, attempt
`serde_json::from_str`. The first record whose `type` field equals
`thread.started` carries the session ID in the `thread_id` field.
The same `thread_id` is echoed back on `codex exec resume <id> --json`,
which is what makes the round-trip stable.

**Important nuance — field naming:** codex calls it `thread_id`, not
`session_id`. Our schema (per spec § 5.1) stores it under `session_id`
on each `codex` turn record; this is just the canonical name we
chose for it in our wire format. They refer to the same UUID.

### Why method A over the alternatives

- **Method A (chosen):** Structured JSONL on stdout. Field name is
  stable, line is the very first record, easy to parse without a regex.
  Future-proof: if codex 0.131+ adds extra preamble events, finding
  the first record with `type == "thread.started"` is robust.

- **Method B (stderr regex on `session id: <uuid>`):** Also works
  empirically — the codex preamble box on stderr always contains a
  `session id: <uuid>` line. Could be used as a *fallback* if
  `--json` is ever unavailable. Less robust because it's
  presentation-formatted (the preamble box is a CLI banner intended
  for humans), so any banner refresh could break it. We will not use
  it in v1 because method A is strictly cheaper and more stable.

- **Method C (alternate stderr format):** Not needed; method B's
  surface is identical to the format the template called out.

- **Method D (no native resume — replay-context only):** Not needed.
  Resume works end-to-end (see acceptance evidence below).

## Acceptance evidence

`CODEX_AVAILABLE=1 cargo test --test codex_contract -- --nocapture
--test-threads=1` produces (against `codex-cli 0.130.0`, 2026-05-17):

### `capture_method_json` (PASS)

```
exit=Some(0)
--json carries thread_id: true
--json carries session_id: false
--- stdout ---
{"type":"thread.started","thread_id":"019e373e-ef1d-7f42-a013-497144960d5c"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"hi"}}
{"type":"turn.completed","usage":{"input_tokens":44189,"cached_input_tokens":3456,"output_tokens":19,"reasoning_output_tokens":17}}
```

### `capture_method_stderr_regex` (PASS — informational only)

```
stderr session line found: Some("session id: 019e373f-3d05-7c52-b347-aa7e2c066ce2")
--- full stderr ---
Reading additional input from stdin...
OpenAI Codex v0.130.0
--------
workdir: /...
model: gpt-5.5
provider: openai
approval: never
sandbox: workspace-write [...]
reasoning effort: high
reasoning summaries: none
session id: 019e373f-3d05-7c52-b347-aa7e2c066ce2
--------
user
reply with exactly the word 'hi'
codex
hi
tokens used
287
```

### `resume_round_trip` (PASS)

Captured `thread_id: 019e373f-5e43-7153-96bf-7c796e6176a3` from the first
`codex exec --json` and round-tripped to `codex exec resume <id> --json`:

```
{"type":"thread.started","thread_id":"019e373f-5e43-7153-96bf-7c796e6176a3"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"4242"}}
{"type":"turn.completed","usage":{"input_tokens":88561,"cached_input_tokens":50432,"output_tokens":146,"reasoning_output_tokens":141}}
```

The resumed turn returns `4242` — the number the original turn was told
to remember. Native resume works end-to-end.

### Total run time

53.1 seconds wall-clock for all 4 tests (sequential, real network calls
to ChatGPT). Tests are gated on `CODEX_AVAILABLE=1` so they don't run in
CI without an authenticated codex on PATH.

## Session-expired surface

When `codex exec resume <uuid>` is called with a well-formed UUID that
does not correspond to a stored rollout, codex emits:

- **Exit code:** `1`
- **Stdout:** empty
- **Stderr (single line):**
  ```
  Error: thread/resume: thread/resume failed: no rollout found for thread id <uuid> (code -32600)
  ```

The `providers/codex.rs` followup impl (Task 10) detects this fallback
condition by matching:

1. `exit_status != 0`, AND
2. `stderr.contains("no rollout found for thread id")`

When both match, the provider treats the captured `session_id` as
expired and re-runs `codex exec --json` from scratch with a
replay-context prompt (i.e. it inlines the prior conversation into the
new prompt), and persists the new `thread_id` to the next turn's
JSONL record. This is the same fallback shape that the spec mandates
for unleash (which never has native resume).

**Non-UUID resume tokens:** When a non-UUID string is passed (e.g.
`codex exec resume "invalid-session-id-xxx"`), codex *does not* error.
Per `codex exec resume --help` ("UUIDs take precedence if it parses"),
it falls through to thread-name lookup and silently starts a fresh
session, returning exit 0 with a freshly-allocated `thread_id`. Our
schema only ever stores UUIDs (because that's what we captured from
the previous `thread.started` event), so this edge case does not
arise in our caller — but it's worth being aware of for tests and for
any future migration parser that might encounter non-UUID values.

## Risks and version-stability notes

- **codex 0.130.0 specific.** The acceptance evidence above is from
  codex-cli 0.130.0. The `thread.started` JSONL event has been stable
  since codex introduced `--json` for `exec`, but a future version
  could rename the event type or rename `thread_id`. Mitigations:
  - The contract test (`capture_method_json`) asserts
    `stdout.contains("\"thread_id\"")` — it will fail loudly on a
    rename, surfacing the need to revisit this doc.
  - The `resume_round_trip` test exercises the *behavior* end-to-end
    (memory of "4242"), so even if the field name changes, a broken
    capture will be caught by the round-trip assertion.
- **Org-deployed codex versions.** Spec § 12.1 raises the question of
  whether codex versions differ across the org. This decision uses
  the version that ships via `brew install codex` as of 2026-05-17.
  When the codex provider in `triage-cli-rs` ships to other analysts,
  the contract test should be re-run on their machine before relying
  on native resume; otherwise the unleash-style replay-context
  fallback (method D) is the safe default.
- **`-c model_supports_reasoning_summaries`-style config overrides**
  could in principle suppress the preamble, but they do not affect
  the JSONL stream on stdout. Method A is the most insulated from
  config drift.

## Related code

- Contract tests: `triage-cli-rs/tests/codex_contract.rs`
- Existing codex provider (target of Task 10 changes):
  `triage-cli-rs/src/providers/codex.rs`
- Spec sections discharged: § 5.6, § 12.1
