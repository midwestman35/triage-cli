# Interactive Investigation — Design Spec

**Status:** Approved 2026-05-17 (brainstorming session); pending implementation plan.
**Owner:** enrique
**Related:**
- `docs/spec/v1-reframe.md` — the authoritative v1 contract this spec extends
- `docs/ROADMAP.md` — tracking entry for this feature and its automation follow-ons

---

## 1. Goal

Give NOC analysts an interactive way to revisit an existing investigation
inside `triage-cli` after the initial structured pipeline run, without
restarting from zero. The motivating workflow: an analyst opens ticket
44776, runs `investigate`, sends the customer-facing reply, then waits.
When the customer brings back new evidence days later, the analyst should
be able to feed the new logs into the same LLM context, ask follow-up
questions ("what changed and what conclusions can you draw?"), and
optionally re-emit the five-markdown folder with the updated
understanding — all while staying inside the existing inbox TUI.

## 2. Non-goals

- Autonomous Zendesk / Jira writes from chat (the CONFIRM-gated drafts
  contract still holds for any externally-visible output).
- Replacing the single-shot `investigate` / `triage` / `watch` pipeline.
  The interactive feature is additive; the structured pipeline remains
  the only producer of the five-markdown contract.
- Multi-analyst real-time collaboration. CONVERSATION.md is single-writer
  per session; concurrent edits are out of scope.
- Provider-agnostic native session resume. Codex gets native resume;
  unleash and any other provider fall back to replay-context (same single-
  turn semantics as today). Cross-provider session migration is explicitly
  refused with a banner (see section 9.2).

## 3. Confirmed decisions

These were resolved during the 2026-05-17 brainstorming session. The
written design below reflects them.

| Question | Decision |
|---|---|
| Where does the feature live? | **Inbox TUI chat pane** (sixth tab alongside the existing five). |
| Where does conversation state live? | **Hybrid** — triage-cli owns `CONVERSATION.md` as a human-readable transcript; codex owns short-term context via session resume. Replay-context is the fallback. |
| When does the five-markdown folder mutate? | **Explicit `/revise` slash-command only.** Chat turns are append-only by default. |
| Provider mismatch on open? | **Refuse with a banner**, require explicit `[s]` to start a fresh session under the active provider. No silent fallback. |
| Slash-command discoverability? | **Persistent command bar** at the bottom of the chat pane (color-coded). No `/help`-only discovery. |
| Color styling? | Greens / reds / yellow per status class; animated gradient spinner while codex is thinking. Specific palette in section 5.3. |

## 4. Architecture

One new module (`tui/chat.rs`), one new pipeline entry (`pipeline::followup_turn`),
one trait method addition (`LlmProvider::followup` with a default impl).
Everything else extends existing modules.

```
                                                       +-- existing --+
                                                       | investigate  |
                                                       | (single-shot)|
+-- new -----------------+   +-- existing ----------+  +--------------+
| tui/chat.rs            |   | tui/inbox.rs         |
|   ChatPane             |<->|   InboxApp (extended |          ^
|     transcript view    |   |   with chat tab;     |          | /revise
|     input modal        |   |   keybinding `a`)    |          |
|     command bar        |   +----------------------+          |
+------------------------+                                     |
            |                                                  |
            v                                                  |
+-- new ------------------+   +-- existing ------+             |
| chat.rs (logic)         |-->| ticket_folder.rs |             |
|   CONVERSATION.md       |   |   reused as the  |             |
|   schema, parse, write; |   |   shared atomic- |             |
|   slash-command parser; |   |   write helper   |             |
|   evidence intake;      |   |   (tempfile +    |             |
|   session manifest      |   |   rename)        |             |
+-------------------------+   +------------------+             |
            |                                                  |
            v                                                  |
+-- new pipeline entry --+    +-- new provider surface --+     |
| pipeline::followup_turn|--->| LlmProvider::followup    |     |
|   call provider.followup    |   - codex: codex exec    |     |
|   append to CONVERSATION    |     resume <id> w/       |     |
|   .md; on /revise call:     |     replay fallback      |     |
|------------------------|    |   - unleash (default):   |     |
                              |     replay-context       |     |
            |                 +--------------------------+     |
            +-- on /revise: invoke ----------------------------+
                investigate_one_structured(
                    bundle_with_followup_evidence,
                    rubric, followup_mode=true)
                under existing soft-lock rules
```

The single inviolable principle: **the structured five-markdown pipeline
stays the only path that mutates the contract-protected files.** `/revise`
re-enters it; it does not parallel-implement it.

**Ownership clarification:** `chat.rs` owns the `CONVERSATION.md` schema,
parser, and writer. `ticket_folder.rs` is **not** extended with
CONVERSATION-specific logic — it only exposes its existing atomic-write
helper (`tempfile + rename`) as a shared utility that `chat.rs` reuses.
The reverse coupling (ticket_folder.rs depending on chat.rs) is forbidden:
the contract-protected five-markdown writer must not know that
CONVERSATION.md exists.

## 5. Data shapes

### 5.1 CONVERSATION.md

Lives at `Tickets/<id>/CONVERSATION.md`. Markdown that is both human-readable
and machine-parseable by `chat::parse_conversation`. Each turn is a
`## turn-NNN` block; the body after the `---` separator is verbatim.

```markdown
<!-- triage-cli conversation v1 -->
<!-- ticket_id: 44776 -->

## turn-001 analyst 2026-05-15T14:20:13Z
attached_files:
  - station.log (8.1 KB, sha256:7c4e...)
attached_pastes: []
---
Customer reports audio dropped at 14:32 PT today.

## turn-002 codex 2026-05-15T14:21:02Z provider=codex model=gpt-5.5 tokens=4200/980 elapsed_s=4.1
session_id: 01HZ8K2W3X4Y5Z6
---
Initial hypothesis: vendor SBC misconfiguration. Fork B...

## turn-003 analyst 2026-05-17T09:14:50Z
attached_files:
  - customer-station-2026-05-17.log (12.4 KB, sha256:9af1...)
attached_pastes:
  - customer-note: "rebooted twice during the call"
---
Customer brought back the attached log after my reply. What changed
and does the fork still hold?

## turn-004 codex 2026-05-17T09:14:54Z provider=codex model=gpt-5.5 tokens=1850/620 elapsed_s=4.1 resumed=true
session_id: 01HZ8K2W3X4Y5Z6
---
The new log shows the station rebooting twice during the incident
window. That moves us off fork B (vendor) and toward fork A
(engineering) because the reboot timing matches the console-watchdog
pattern in E-007...

## turn-005 system 2026-05-17T09:16:12Z action=revise outcome=success
diff:
  - fork: B -> A
  - confidence: med -> high
  - rubric_row: "vendor-sbc-drop-001" -> "console-watchdog-reboot-007"
---
Five-markdown folder regenerated using turns 003-004 as supplementary
evidence. STATE.md.updated_at bumped to 2026-05-17T09:16:12Z.
```

**Schema invariants:**

- `turn_kind` ∈ `{analyst, codex, system, automated}`. `automated` is
  reserved for the watcher / cron integrations covered in section 11.
- Turn numbers are zero-padded (three digits), strictly monotonic, never
  reused.
- The blank `---` line separates front-matter from body. The body is
  verbatim — codex output is not reformatted on disk.
- Attachments referenced in a turn are copied into
  `Tickets/<id>/attachments/turn-NNN/<basename>` so the conversation is
  reproducible offline. Sources already inside `Tickets/<id>/` are not
  copied (they are referenced by relative path).
- File reads are append-only after parse — we never rewrite earlier turns.
  This makes the file safe to tail and avoids torn-write edge cases.

### 5.2 `.session/` directory

```
Tickets/44776/.session/
  codex-session-id        # plaintext, single line, e.g. "01HZ8K2W3X4Y5Z6"
  last-resumed-at         # plaintext ISO 8601 UTC
  manifest.json           # provider provenance
```

`manifest.json` shape:

```json
{
  "version": 1,
  "provider": "codex",
  "model": "gpt-5.5",
  "created_at": "2026-05-15T14:21:02Z",
  "last_resumed_at": "2026-05-17T09:14:54Z",
  "resume_count": 1
}
```

The directory is provider-tagged. On chat-pane open, if
`manifest.provider != LLM_PROVIDER`, the pane displays a red banner and
halts until the analyst either changes `LLM_PROVIDER` and reopens, or
presses `[s]` to explicitly start a fresh session under the active
provider (which preserves `CONVERSATION.md` and creates a new manifest).

### 5.3 Visual style

Color tokens (`ratatui::style::Color::Rgb`):

| Token | Color | Purpose |
|---|---|---|
| `analyst_header` | `#7ec8ff` (muted cyan) | Analyst turn front-matter line |
| `codex_header` | `#6fdc8c` (green) | Codex turn front-matter line; `◆` glyph prefix |
| `system_header` | `#ffb86c` (amber) | System turn (revise, fallback, mismatch) |
| `automated_header` | `#bd93f9` (violet) | Reserved for automated turns (section 11) |
| `success` | `#3fbf3f` | `✓` lines, send-confirmed feedback |
| `failure` | `#ff5555` | `✗` lines, hard errors |
| `soft_warn` | `#f1fa8c` | `⚠` lines, soft warnings |
| `spinner_gradient` | `#7ec8ff` → `#6fdc8c` → `#7ec8ff` | 200ms-cycle animated gradient on the thinking indicator |
| `cmd_key` | `#3fbf3f` | Slash-command keys in the command bar |
| `cmd_desc` | `#888888` | Slash-command descriptions in the command bar |

### 5.4 Provider trait extension

`providers/mod.rs`:

```rust
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn complete<'a>(/* unchanged */) -> ...;

    /// Optional follow-up surface. Default: ignore session_id and call
    /// complete() with the caller-supplied replay prompt. Providers with
    /// native session resume (codex) override this method.
    fn followup<'a>(
        &'a self,
        session_id: Option<&'a str>,
        prompt: &'a str,
        system_prompt: &'a str,
        model: &'a str,
        attachments: &'a [Attachment],
    ) -> Pin<Box<dyn Future<Output = Result<FollowupResult, ProviderError>> + Send + 'a>> {
        // default impl: replay-context single-turn
    }
}

pub struct FollowupResult {
    pub text: String,
    pub tokens_in: Option<u32>,
    pub tokens_out: Option<u32>,
    pub session_id: Option<String>,
    pub resumed: bool,
}
```

- **Codex impl:** when `session_id` is `Some`, calls
  `codex exec resume <id> --model <m> "<prompt>"`. Image attachments use
  `-i <path>` flags. On `session not found` from codex stderr, falls back
  to `codex exec` with replay prompt. Extracts the new session ID from
  codex's stderr `session_id=...` line.
- **Unleash impl:** uses the default trait method (replay-context, no
  native session). Returns `session_id: None, resumed: false`.

No `async-trait` crate — same `Pin<Box<dyn Future>>` pattern the existing
`complete` method uses (`providers/mod.rs:31-39`).

## 6. UX

### 6.1 Layout

The chat lives as a sixth tab alongside `INTAKE | EVIDENCE | FORK |
DRAFTS | STATE | CHAT`. Pressing `a` from the ticket list jumps directly
into CHAT and focuses the input modal; `Tab` from inside CHAT cycles
through file tabs the normal way.

The pane is split horizontally: transcript above, input modal + command
bar below. The command bar is always visible and color-coded.

```
+-- Inbox (4 unread) ----+-- Ticket 44776 ---------------------------+
|  44776  B  med  enrique| [intake|evid|fork|drafts|state| CHAT* ]   |
|  44688  A  high enrique|                                            |
|  44801  D  low  marcus | enrique 14:20Z (turn-001) attached:1       |
|  44823  C  med  enrique|   Customer reports audio dropped at        |
|                        |   14:32 PT today.                          |
| [↑↓] navigate          |                                            |
| [r]  refresh           | codex 14:21Z (turn-002) ◆ 4.1s 4200/980    |
| [a]  ASK follow-up     |   Initial hypothesis: vendor SBC...        |
| [Tab] cycle tabs       |                                            |
| [o]  open in zendesk   | enrique 09:14Z (turn-003) attached:1+1     |
| [q]  quit              |   Customer brought back the attached log   |
|                        |   after my reply. What changed?            |
| ─ session ─            |                                            |
| provider: codex        | codex 09:14Z (turn-004) ◆ 4.1s resumed     |
| model:    gpt-5.5      |   The new log shows the station rebooting  |
| turns:    4            |   twice during the incident window...      |
| sid:    01HZ8K2W…      |                                            |
|                        | system 09:16Z (turn-005) ✓ revise B→A      |
|                        |   Five-markdown folder regenerated.        |
|                        |                                            |
|                        | +-- ASK (Ctrl-S send, Esc cancel) --------+|
|                        | | _                                       ||
|                        | |                                         ||
|                        | +-----------------------------------------+|
|                        | /file [^F]  /paste [^V]  /revise [^R]      |
|                        | /edit [^E]  /retry [^T]  /quit [^C]        |
+------------------------+-------------------------------------------+
   ⠹ codex is thinking… 4.1s elapsed (Esc to cancel)
```

### 6.2 Keybindings inside the chat tab

| Key | Action |
|---|---|
| `Ctrl-S` | Send turn (or `Enter` for single-line input; `Shift+Enter` newline) |
| `Ctrl-F` | Attach file (file picker rooted at `./` with arrow-keys + `/` filter) |
| `Ctrl-V` | Paste evidence with label (`label=` prefix then body) |
| `Ctrl-E` | Open `$EDITOR` (suspend the TUI, edit in vim/nano/$EDITOR, return) |
| `Ctrl-R` | `/revise` — re-run the structured pipeline |
| `Ctrl-T` | Retry the last codex call |
| `Ctrl-C` / `Esc` | Cancel in-flight call or close the modal |
| `q` | (modal closed) return to the inbox list |

### 6.3 New Cargo dependencies

| Crate | Version | Purpose |
|---|---|---|
| `tui-textarea` | `0.7` | Multiline editable widget with Vim/Emacs key bindings |
| `throbber-widgets-tui` | `0.7` | Gradient spinner during in-flight codex calls |

No other new dependencies. Existing `ratatui`, `crossterm`, `tokio`,
`indicatif`, `thiserror`, `serde`, `chrono` cover the rest.

## 7. Dataflow

### 7.1 Normal turn

```
1. Analyst presses [a] on ticket 44776 in inbox.
2. ChatPane reads:
     Tickets/44776/CONVERSATION.md  -> Vec<Turn>
     Tickets/44776/.session/manifest.json -> provider check
   If manifest.provider != LLM_PROVIDER -> show mismatch banner, halt.
3. Analyst types, attaches files, hits Ctrl-S.
4. ChatPane sends ChatEvent::SubmitTurn { body, attachments } to a
   tokio task. The chat task:
     a. Writes the analyst turn to CONVERSATION.md atomically (tempfile
        + rename, appending to existing).
     b. Copies attachments into Tickets/44776/attachments/turn-NNN/.
        Sources already under Tickets/44776/ are referenced by relative
        path, not copied.
     c. Calls pipeline::followup_turn(ticket_id, session_id, prompt,
        attachments, &provider). This function:
          - reads session_id from .session/codex-session-id (or None)
          - calls provider.followup(session_id, prompt, sys, model, atts)
          - returns FollowupResult (text + session_id + resumed flag)
     d. Streams elapsed-time updates through a tokio channel
        (existing ChannelReporter pattern) so the gradient spinner
        animates with real elapsed counts.
     e. On success: writes the codex turn to CONVERSATION.md, updates
        .session/last-resumed-at and manifest.json (bump resume_count).
     f. On failure: writes a system turn noting the failure, with the
        provider error text; offers /retry.
5. ChatPane reads the new Turn struct from the channel and re-renders
   without re-parsing the whole file.
```

### 7.2 Revise turn

```
1. Analyst types /revise (or Ctrl-R) in the input modal.
2. Chat task validates: at least one new analyst turn since last revise?
   If not: refuses with "no new evidence since last revise; nothing to do".
3. Chat task builds a synthetic TriageBundle:
     - existing INTAKE/EVIDENCE_PREFLIGHT facts (re-fetched from disk)
     - attachments from turns since last revise added as
       InvestigationEvidence::local_files
     - body text from analyst turns added as PastedEvidence
     - prior codex turns are NOT re-fed (the structured pipeline gets
       the structured-output prompt; we feed it evidence, not chat
       history)
4. Chat task calls pipeline::investigate_one_structured(bundle, rubric,
   followup_mode=true). The followup_mode flag tells the pipeline to:
     - skip Zendesk re-fetch: the caller (chat.rs) reconstructs a Ticket
       struct by re-reading INTAKE.md's "Ticket facts" section plus the
       cached customer-history-evidence stored alongside .session/. If
       reconstruction fails, the pipeline falls back to a live Zendesk
       fetch and writes a soft-warn "INTAKE.md reconstruction failed,
       re-fetched from Zendesk" entry in STATE.md.validator_warnings.
     - keep the existing CONVERSATION.md (don't wipe it on rewrite)
     - record a "revised from turns X..Y" entry in
       STATE.md.validator_warnings
5. The five-markdown folder is rewritten atomically (same soft-lock path
   as today). If soft-lock conflict: write a system turn with the
   existing-owner / new-owner diff; offer [f] to retry with --force,
   [Esc] to cancel.
6. Chat task writes a system turn-NNN to CONVERSATION.md with
   action=revise and the field-level diff.
7. Inbox row 44776 reads STATE.md when the chat pane closes; row updates
   to show the new fork letter with a small "revised" marker.
```

## 8. Error handling

| Failure mode | Detection | Response |
|---|---|---|
| Codex session expired / unrecoverable | `codex exec resume` non-zero with `session not found` on stderr | Fall back to replay-context; log yellow system turn "session lost, replayed N turns"; retain CONVERSATION.md; start fresh session ID on next turn |
| Provider mismatch (manifest.provider ≠ LLM_PROVIDER) | Read at chat pane open | Red banner; `[s]` to start fresh session under new provider, or back out |
| Soft-lock conflict on `/revise` | `TicketFolderError::SoftLockConflict` | System turn with existing-owner / new-owner diff; `[f]` to retry with `--force`, `[Esc]` to cancel |
| Structured-output validation failure after `/revise` | `LlmError::StructuredAfterRetry` | System turn with raw-response stash path; CONVERSATION.md preserved; five-markdown folder NOT rewritten |
| File attach > 1 MB (raw on-disk bytes, before any extraction) | `chat::attach_file` size check via `fs::metadata` | Soft-warn yellow turn; `[f]` to force or `[t]` to truncate the extracted text to the first 256 KB (UTF-8 boundary safe). Binary / zip files are not truncated; force-or-skip only. |
| `$EDITOR` not set / exits non-zero | Subprocess return code | Red toast at the bottom of the modal; input contents preserved verbatim |
| In-flight codex call > 120s | tokio timer | Soft-warn at 60s; red at 120s with `[c]` to cancel (SIGTERM to codex subprocess); system turn recording the cancel |
| `codex` binary not on PATH | `which::which("codex").is_err()` (same as existing codex provider check) | Red banner with install hint; chat pane refuses to accept turns until codex is available or LLM_PROVIDER is changed |

A new `PipelineError::Followup` variant carries the new failure-mode
classes that don't already fit `Zendesk` / `Datadog` / `Llm` / `Extract`
/ `Memory` / `TicketFolder`. Specifically: session-expired-with-no-replay
recovery, and codex-session-id-extraction-failed.

## 9. Soft-lock and contract preservation

### 9.1 What stays unchanged

- The five-markdown folder is the v1 contract surface. `INTAKE.md`,
  `EVIDENCE_PREFLIGHT.md`, `FORK_PACKET.md`, `DRAFTS.md`, `STATE.md`
  retain their existing shapes byte-for-byte.
- The soft-lock check (`STATE.md.owner` + `--force` + `--diff` exit code
  2) runs before any of the five files are touched by `/revise`.
- Validator warnings remain soft-warn, not blocking. A revise that
  produces a rubric-row miss writes the warning into
  `STATE.md.validator_warnings` and continues.
- The PII redaction scope (caller-side only, operational identifiers
  preserved) applies to follow-up prompts before they reach the
  provider, identical to today's bundle redaction.

### 9.2 Net new contract surfaces

- `Tickets/<id>/CONVERSATION.md` — version-tagged via the
  `<!-- triage-cli conversation v1 -->` HTML comment header. Future
  schema changes carry a v2 header and a migration path.
- `Tickets/<id>/.session/` — version-tagged via `manifest.json.version`.
- `Tickets/<id>/attachments/turn-NNN/` — opaque per-turn attachment
  directory; no schema beyond filesystem layout.
- One new `STATE.md.validator_warnings` pattern: `"revised from turns X..Y"`.
  Backward compatible with the existing schema (validator_warnings is
  already a free-form string array).

## 10. Testing

| Layer | What | How |
|---|---|---|
| `chat::parse_conversation` | CONVERSATION.md round-trip: parse → serialize → byte-identical | Inline `#[cfg(test)]` in `chat.rs`. Cover every turn_kind and missing-field defaults. |
| `chat::slash_command` | Slash-command parsing including error cases | Inline tests; table-driven. |
| `providers::codex::followup` | Session-resume happy path + session-lost fallback | Mock codex subprocess via a fixture script on PATH; test-only `MOCK_CODEX_PATH` env var honored under `#[cfg(test)]`. |
| `pipeline::followup_turn` | Append-only behavior, attachment copy, session-id update | Tempdir-based unit test. |
| `pipeline::investigate_one_structured` in followup_mode | Revise path: CONVERSATION.md preserved, attachments folded into bundle, STATE.md.validator_warnings updated | Extend existing fixture-based golden-output test (roadmap #3) with a `with-followup-evidence` fixture. |
| `tui/chat` | Snapshot rendering of the pane with various turn-kind colors and the spinner gradient frame | Ratatui's `TestBackend` plus `insta` (or hand-rolled buffer asserts). |

No network. All codex/unleash calls in tests go through the mock
subprocess or a fake `LlmProvider` impl.

## 11. Automation hooks (forward-looking)

The user has called out automated triage and ticket-fetching as a
near-term follow-on initiative. v1 of this feature exposes three concrete
extension points so that follow-on work doesn't require re-architecting
the chat pane.

### 11.1 `turn_kind: automated` is a real schema value from day one

The parser, renderer, color palette, and CONVERSATION.md schema all
include `automated` as a first-class variant. A future watcher that
detects new Zendesk comments calls
`chat::append_automated_turn(ticket_id, body, attachments)` and the
existing chat pane renders the turn with the violet header. No code path
in the TUI needs to change to add a new automated source.

### 11.2 `pipeline::followup_turn` is a public library function

Not a TUI-only path. A cron-style "auto-summarize new evidence" task can
call `pipeline::followup_turn` directly from `watcher.rs` with
`turn_kind: automated`. The chat pane is the consumer of the result, not
the only producer of work. A watcher integration is purely additive — it
calls the function, writes the turn, and the analyst sees it when they
next open the chat pane.

### 11.3 Slash commands route through a `ChatCommand` enum

Adding a future `/dispatch <jira-id>`, `/notify-team`, or
`/escalate-to-engineering` slash is a single variant addition, not a TUI
rewrite. The same enum is what an automated turn would emit if it wanted
to suggest an action — a future automation that opens a Jira draft and
adds a `system` turn with a "suggested next action: /dispatch JIRA-1234"
line is something the chat pane already knows how to render.

Roadmap entries for the near-term automation work are tracked in
`docs/ROADMAP.md` as a follow-on to this spec.

## 12. Open questions

None blocking. Implementation-level details (exact `tui-textarea`
keybinding map, exact gradient frame count, exact spinner glyph set,
exact INTAKE.md "Ticket facts" parser shape for the followup_mode
reconstruction path) are resolvable at implementation time without
re-opening the design. The writing-plans skill will turn this spec into
a step-by-step implementation plan and address those details there.

## 13. Out of scope (deferred)

| Item | Why not in v1 |
|---|---|
| Multi-analyst concurrent chat | CONVERSATION.md is single-writer; concurrent edits are a separate problem with separate trade-offs. |
| Cross-provider session migration | Refusing with a banner is the simpler correct behavior. Migration can be revisited if a real workflow demands it. |
| Streaming codex output (token-by-token render) | `codex exec` is request/response, not streaming. Streaming is a provider-level feature change, not a chat pane change. |
| Voice input or recorded audio attachments | Operationally interesting; out of scope for the text-first v1. |
| Mobile / web access to the chat pane | The TUI is a terminal surface by design. Remote access lands in the existing `triage-cli remote-control` direction (currently experimental in codex), not here. |

---

**End of spec.**
