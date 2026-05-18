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
- Multi-analyst real-time collaboration in the chat pane. Single-writer
  is enforced by the per-ticket advisory file lock (section 3 +
  section 5.5); concurrent collaborative editing is out of scope.
- Provider-agnostic native session resume. Codex gets native resume;
  unleash and any other provider fall back to replay-context. Cross-
  provider mismatches degrade gracefully rather than hard-refusing
  (section 8).

## 2.5 V1 ship list (the narrow loop)

V1 is the smallest cohesive shipping unit that proves the data model and
the core loop. Polish lives in v2.

**V1 ships:**

- Open the chat tab (`a` from the inbox row).
- Read existing `CONVERSATION.jsonl` and render it as the transcript.
- Append an analyst turn: type body, attach a file by **plain path**
  prompt (no file picker), attach a labeled paste (`label=body`).
- Ask follow-up: call `provider.followup`; render the response turn.
- Retry the last turn on failure.
- `/revise` re-enters the structured pipeline against the JSON snapshot
  (section 5.4) plus new-evidence-since-last-revise.
- Per-ticket advisory file lock; provenance fields on each evidence row.
- Static throbber while a call is in flight (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`-style
  spinner with no gradient).

**Deferred to v2 (and tracked in ROADMAP.md):**

- Custom in-TUI file picker (v1 uses a plain path prompt).
- `$EDITOR` integration / `Ctrl-E` suspend-into-editor.
- Animated gradient spinner (`throbber-widgets-tui` dependency).
- Automation hooks (`turn_kind: automated` writers — but the schema
  value itself ships in v1 so the v2 work is purely additive).
- `Ctrl-V`-prefix paste-with-label modal (v1 uses a single text input
  for the `label=body` paste).

The schema, the lock, the JSON snapshot, and the provider contract all
ship in v1 because retrofitting them later is expensive. The UX polish
items are retrofittable without changing data shapes.

## 3. Confirmed decisions

These were resolved during the 2026-05-17 brainstorming session. The
written design below reflects them.

| Question | Decision |
|---|---|
| Where does the feature live? | **Inbox TUI chat pane** (sixth tab alongside the existing five). |
| Where does conversation state live? | **JSONL source of truth + rendered markdown.** `CONVERSATION.jsonl` is the machine-readable log (one JSON object per turn) and is the contract; `CONVERSATION.md` is **derived** from it and exists for human readability. The pipeline never parses the markdown. |
| Where does the durable ticket snapshot live? | `Tickets/<id>/.session/base-ticket.json` and `Tickets/<id>/.session/base-evidence-manifest.json`, written atomically at the end of the original `investigate` run. `/revise` rebuilds the bundle from these JSON snapshots plus new evidence. **No markdown reconstruction.** Missing snapshots → live Zendesk re-fetch with a soft-warn. |
| When does the five-markdown folder mutate? | **Explicit `/revise` slash-command only**, and only when there is **new evidence** since the last revise (attached files or labeled pastes — a new question alone is not enough). |
| Provider mismatch on open? | **Graceful degradation.** Try the manifest's resume primitive when the active provider matches; otherwise replay under the active provider with a yellow banner. Hard-refuse only when the analyst has explicitly opted into "native resume required" mode. |
| Slash-command discoverability? | **Persistent command bar** at the bottom of the chat pane (color-coded). No `/help`-only discovery. |
| Color styling? | Greens / reds / yellow per status class. **V1 ships a static throbber**; the animated gradient spinner is deferred to v2. |
| Concurrency model? | **Per-ticket advisory file lock** at `Tickets/<id>/.session/lock` held by the chat pane (and by any future automated writer) for the duration of a turn write. Held with `fs2::FileExt::try_lock_exclusive` (or platform equivalent). |

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
|   CONVERSATION.jsonl    |   |   reused as the  |             |
|   schema, append, read; |   |   shared atomic- |             |
|   CONVERSATION.md       |   |   write helper   |             |
|   renderer (derived);   |   |   (tempfile +    |             |
|   slash-command parser; |   |   rename) +      |             |
|   evidence intake +     |   |   advisory lock  |             |
|   provenance;           |   |   helper         |             |
|   session manifest;     |   +------------------+             |
|   per-ticket lock       |                                    |
+-------------------------+                                    |
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

**Ownership clarification:** `chat.rs` owns the `CONVERSATION.jsonl`
schema, parser, and append-writer; it also owns the `CONVERSATION.md`
renderer (which derives the markdown from the JSONL — no manual editing
of the markdown is supported or expected). `ticket_folder.rs` is **not**
extended with CONVERSATION-specific logic — it only exposes its existing
atomic-write helper (`tempfile + rename`) and a new advisory-lock helper
as shared utilities that `chat.rs` reuses. The reverse coupling
(`ticket_folder.rs` depending on `chat.rs`) is forbidden: the
contract-protected five-markdown writer must not know that
`CONVERSATION.jsonl` exists.

**Why JSONL not pure JSON:** append is the only mutation operation on
the conversation log, so a line-delimited format avoids the
read-whole-file → mutate → write-whole-file round-trip that a top-level
JSON array would require, and gives us crash-safe writes (a torn final
line is detectable and discardable; the prior turns remain intact).

## 5. Data shapes

### 5.1 CONVERSATION.jsonl (source of truth)

Lives at `Tickets/<id>/CONVERSATION.jsonl`. One JSON object per line,
strictly append-only. This is the machine-readable contract — every
parser, renderer, and downstream consumer reads JSONL, never markdown.
A torn final line (process killed mid-write) is detectable by the parser
and skipped; the prior turns remain intact.

```jsonl
{"schema":"triage-cli/conversation","schema_version":1,"ticket_id":"44776","turn":1,"turn_kind":"analyst","ts":"2026-05-15T14:20:13Z","author":"enrique","body":"Customer reports audio dropped at 14:32 PT today.","evidence":[{"kind":"file","source_path":"./station.log","copied_path":"Tickets/44776/attachments/turn-001/station.log","basename":"station.log","sha256":"7c4e...","bytes":8294,"detected_type":"log","extraction":"full","truncated":false,"sent_to_provider":true}]}
{"schema":"triage-cli/conversation","schema_version":1,"ticket_id":"44776","turn":2,"turn_kind":"codex","ts":"2026-05-15T14:21:02Z","provider":"codex","model":"gpt-5.5","tokens_in":4200,"tokens_out":980,"elapsed_s":4.1,"session_id":"01HZ8K2W3X4Y5Z6","resumed":false,"body":"Initial hypothesis: vendor SBC misconfiguration. Fork B..."}
{"schema":"triage-cli/conversation","schema_version":1,"ticket_id":"44776","turn":3,"turn_kind":"analyst","ts":"2026-05-17T09:14:50Z","author":"enrique","body":"Customer brought back the attached log after my reply. What changed and does the fork still hold?","evidence":[{"kind":"file","source_path":"./customer-station-2026-05-17.log","copied_path":"Tickets/44776/attachments/turn-003/customer-station-2026-05-17.log","basename":"customer-station-2026-05-17.log","sha256":"9af1...","bytes":12685,"detected_type":"log","extraction":"truncated","truncated":true,"sent_to_provider":true,"truncation_note":"extracted text truncated to first 256 KB"},{"kind":"paste","label":"customer-note","body":"rebooted twice during the call","bytes":31,"sent_to_provider":true}]}
{"schema":"triage-cli/conversation","schema_version":1,"ticket_id":"44776","turn":4,"turn_kind":"codex","ts":"2026-05-17T09:14:54Z","provider":"codex","model":"gpt-5.5","tokens_in":1850,"tokens_out":620,"elapsed_s":4.1,"session_id":"01HZ8K2W3X4Y5Z6","resumed":true,"body":"The new log shows the station rebooting twice during the incident window..."}
{"schema":"triage-cli/conversation","schema_version":1,"ticket_id":"44776","turn":5,"turn_kind":"system","ts":"2026-05-17T09:16:12Z","action":"revise","outcome":"success","drove_revision_from_turns":[3,4],"diff":{"fork":["B","A"],"confidence":["med","high"],"rubric_row":["vendor-sbc-drop-001","console-watchdog-reboot-007"]},"body":"Five-markdown folder regenerated using turns 003-004 as supplementary evidence. STATE.md.updated_at bumped to 2026-05-17T09:16:12Z."}
```

**Schema invariants:**

- Every line is a JSON object with `schema`, `schema_version`, `ticket_id`,
  `turn` (1-indexed monotonic integer), `turn_kind`, `ts` (ISO 8601 UTC),
  and `body` fields. Additional fields are turn-kind specific.
- `turn_kind` ∈ `{analyst, codex, system, automated}`. `automated` is
  reserved for the watcher / cron integrations covered in section 11.
- Turn numbers are strictly monotonic, never reused. The per-ticket lock
  (section 5.5) is held during the read-tail → assign-next-turn → append
  sequence.
- Append-only after write — earlier turns are never rewritten. Crash-safe
  by construction: a torn final line is detectable by JSON parse failure
  and skipped on the next read.
- The `evidence` array per analyst / automated turn carries explicit
  provenance per item (section 5.3).

### 5.2 CONVERSATION.md (derived, human-readable)

`CONVERSATION.md` is rendered from `CONVERSATION.jsonl` whenever the
JSONL changes. It is **not** parsed by any code path; the markdown is
a presentation artifact for analysts who want to `cat` or grep the
conversation without running the CLI.

The renderer regenerates the whole file atomically (`tempfile + rename`)
from the JSONL on each append; for a typical 10-turn conversation this
costs ~5 ms. Header lines are color-coded only inside the TUI — the
on-disk markdown is plain text with section headers and the body
verbatim. The renderer is idempotent: the same JSONL always produces
the same markdown bytes.

If `CONVERSATION.md` is missing or out of date with the JSONL (mtime
older than JSONL mtime), the renderer regenerates it lazily on the next
chat-pane open.

### 5.3 Evidence provenance

Every evidence row inside a `turn.evidence` array carries explicit
provenance so audit trails are unambiguous about what the LLM actually
saw versus what was attached.

```json
{
  "kind": "file",
  "source_path": "./customer-station-2026-05-17.log",
  "copied_path": "Tickets/44776/attachments/turn-003/customer-station-2026-05-17.log",
  "basename": "customer-station-2026-05-17.log",
  "sha256": "9af1...",
  "bytes": 12685,
  "detected_type": "log",
  "extraction": "truncated",
  "truncated": true,
  "truncation_note": "extracted text truncated to first 256 KB",
  "sent_to_provider": true
}
```

| Field | Required? | Purpose |
|---|---|---|
| `kind` | yes | `file` \| `paste` |
| `source_path` | files only | Analyst-supplied path before copy. Useful for "where did this come from?" audits. |
| `copied_path` | files only | Path inside `Tickets/<id>/attachments/turn-NNN/` where the bytes live after copy. The only path the LLM ever sees in the prompt. |
| `basename` | files only | Filename without directory. |
| `sha256` | files only | Hex digest of the raw on-disk bytes (not the extracted text). |
| `bytes` | yes | Raw on-disk byte count for files; UTF-8 byte count for pastes. |
| `detected_type` | files only | One of the existing `FileType` variants (`log`/`text`/`json`/`zip`/`unknown`). |
| `extraction` | files only | `full` \| `truncated` \| `binary-skipped`. Describes how the extracted-text representation relates to the raw bytes. |
| `truncated` | files only | `true` iff `extraction == "truncated"`. Convenience field for renderers. |
| `truncation_note` | optional | Free-form explainer when `truncated == true`. |
| `sent_to_provider` | yes | `true` iff the content actually made it into the prompt sent to the LLM (after PII redaction and size gates). False covers cases like binary-skipped or analyst-cancelled-before-send. |
| `label` | pastes only | The `label=` prefix the analyst supplied. |
| `body` | pastes only | Verbatim paste contents (not separately stored on disk). |

**Why we keep raw `bytes` and `sha256` even for already-truncated
content:** auditability. If a customer later asks "what file did you
see when you concluded fork A?", `sha256` plus `copied_path` resolve it
to a specific artifact on disk. Without the digest, post-hoc
reconstruction is ambiguous when files share basenames across tickets.

### 5.4 `.session/` directory (manifests, snapshots, lock)

```
Tickets/44776/.session/
  manifest.json                  # session provenance (provider, model, resume state)
  codex-session-id               # codex-provider only: bare session ID string
  last-resumed-at                # plaintext ISO 8601 UTC
  base-ticket.json               # SNAPSHOT: Ticket struct as fetched on first investigate
  base-evidence-manifest.json    # SNAPSHOT: original evidence bundle, with provenance
  lock                           # advisory file lock (fs2::FileExt::try_lock_exclusive)
```

**`manifest.json` shape:**

```json
{
  "version": 1,
  "provider": "codex",
  "model": "gpt-5.5",
  "created_at": "2026-05-15T14:21:02Z",
  "last_resumed_at": "2026-05-17T09:14:54Z",
  "resume_count": 1,
  "codex_capture_method": "stderr_session_id_line"
}
```

The `codex_capture_method` field documents how the session ID was
extracted from the codex subprocess (see section 5.6 — the codex contract
gate). Future capture methods (e.g. `codex_json_output`) write a
different value here so a session created under one capture path can
be diagnosed if resume later behaves unexpectedly.

**`base-ticket.json` shape:** The exact `Ticket` struct (`models::Ticket`)
as fetched on first `investigate`, serialized via the existing
`#[derive(Serialize)]` impl. Round-trippable; loading it via `serde_json::
from_reader` reconstructs the struct verbatim. Written atomically at the
end of the original `investigate` run by `pipeline::investigate_one_structured`
when `followup_mode == false` (this is the only new write the original
pipeline does).

**`base-evidence-manifest.json` shape:**

```json
{
  "schema": "triage-cli/base-evidence",
  "schema_version": 1,
  "ticket_id": "44776",
  "captured_at": "2026-05-15T14:21:02Z",
  "evidence": [
    {
      "id": "E-001",
      "kind": "zendesk_comment",
      "source": "ticket:44776:comment:0",
      "time_window": null,
      "summary": "Customer first reported audio drop",
      "sha256": "1a2b...",
      "bytes": 412
    },
    {
      "id": "E-007",
      "kind": "datadog_log",
      "source": "datadog:site=us-nv-nvdps-apex:window=2026-05-15T14:30..15:30",
      "time_window": "2026-05-15T14:30Z..15:30Z",
      "summary": "Audio handler reset on station NV-12",
      "sha256": "9af1...",
      "bytes": 2104
    }
  ]
}
```

This is the v1-reframe Evidence-ID model (ROADMAP item #3, already
shipped per recent commits) serialized to disk. `/revise` reads it
verbatim and adds new-evidence-since-last-revise as additional rows.

**`lock` file:** Empty file; opened by chat.rs (and by any future
automated writer) with `fs2::FileExt::try_lock_exclusive` for the
duration of a turn write. On contention, the chat pane shows a yellow
"another writer holds the lock; retrying" banner and retries with
backoff up to 5s, then surfaces a hard error.

### 5.5 Per-ticket advisory lock

The lock protects the read-tail → assign-next-turn → append sequence on
`CONVERSATION.jsonl`, the `base-*.json` snapshot writes, and the
`manifest.json` updates. It does **not** protect the five-markdown
folder (that has its own existing soft-lock semantics on `STATE.md`).

Lock acquisition is `try_lock_exclusive` with a 5s overall budget;
unlock happens automatically when the `File` handle is dropped (RAII).
The lock is advisory (cooperating writers), not enforced by the kernel
against unrelated processes — analysts editing `CONVERSATION.jsonl` by
hand are still possible and are out of contract.

**Writers required to hold the lock:**

- `chat::append_turn` (analyst, codex, system, automated)
- `pipeline::followup_turn` (calls into `chat::append_turn`)
- The future watcher integration (`turn_kind: automated`)

**Writers that do NOT hold this lock** (they have their own existing
mechanisms):

- `pipeline::investigate_one_structured` (uses the existing STATE.md
  soft-lock and atomic five-file rename)
- `ticket_folder::write_ticket_folder` (existing soft-lock)

### 5.6 Codex contract gate (prerequisite)

The spec's earlier draft assumed codex prints `session_id=...` on stderr
on every `codex exec` and `codex exec resume` call. This assumption is
**unverified** and must be discharged before the codex `followup` impl
is implementation-ready.

**Acceptance criteria for the gate** (work item #0 in the implementation
plan, blocks all subsequent codex-followup work):

1. Determine whether `codex exec --json` or `codex exec` carries the
   session ID in a stable, parseable form. If `--json` exposes
   `{"session_id": "..."}` in either the first or last record, use that
   and record `codex_capture_method: codex_json_output` in the manifest.
2. If `--json` is not available or does not carry the session ID,
   produce a reproducible test that runs `codex exec` against a known
   prompt and asserts the format of the session ID line in stderr.
   Store the regex / parser in `providers/codex.rs` and gate it behind
   a `#[test]`-only fixture that simulates the codex output.
3. Verify `codex exec resume <id>` accepts the captured ID and returns
   the same (or a forked) session ID on subsequent calls.
4. Verify session expiry behavior: what does codex print when a
   session ID is no longer valid? The provider's "session not found"
   fallback path depends on this string being stable enough to match
   on. If it is not, fall back to "any non-zero exit + replay" rather
   than "specific stderr match → replay".

If the gate fails (no stable session-ID surface), the spec degrades
gracefully: codex `followup` falls back to **replay-context** like
unleash, and the v1 ships without native session continuity. The chat
loop, JSONL schema, lock, and `/revise` path all still work — they are
provider-agnostic by design.

### 5.7 Visual style

Color tokens (`ratatui::style::Color::Rgb`). All ship in v1 except the
animated gradient spinner, which is deferred to v2.

| Token | Color | Purpose | V1? |
|---|---|---|---|
| `analyst_header` | `#7ec8ff` (muted cyan) | Analyst turn front-matter line | yes |
| `codex_header` | `#6fdc8c` (green) | Codex turn front-matter line; `◆` glyph prefix | yes |
| `system_header` | `#ffb86c` (amber) | System turn (revise, fallback, mismatch) | yes |
| `automated_header` | `#bd93f9` (violet) | Reserved for automated turns (section 11) | yes (schema only; no writer in v1) |
| `success` | `#3fbf3f` | `✓` lines, send-confirmed feedback | yes |
| `failure` | `#ff5555` | `✗` lines, hard errors | yes |
| `soft_warn` | `#f1fa8c` | `⚠` lines, soft warnings | yes |
| `static_throbber` | inherits `codex_header` | `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` braille spinner (no color cycling) while a codex call is in flight | yes |
| `spinner_gradient` | `#7ec8ff` → `#6fdc8c` → `#7ec8ff` | 200ms-cycle animated gradient | **v2** (deferred) |
| `cmd_key` | `#3fbf3f` | Slash-command keys in the command bar | yes |
| `cmd_desc` | `#888888` | Slash-command descriptions in the command bar | yes |

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
  `codex exec resume <id> --model <m> "<prompt>"`. The session-ID
  capture method (`--json` vs stderr regex vs fallback-to-replay) is
  determined by the codex contract gate (section 5.6) **before**
  the impl is considered ready. The implementation records its capture
  method in `manifest.codex_capture_method`.
- **Unleash impl:** uses the default trait method (replay-context, no
  native session). Returns `session_id: None, resumed: false`.
- **Image attachments via `-i <path>`** are deferred to v2. V1 codex
  followup sends text-only prompts; if attachments are non-text
  (zip, binary), the chat pane extracts the text portion via the
  existing `interactive::detect_file_type` + `read_text_if_supported`
  helpers before sending, and the zip-walk extraction (currently
  `TODO(human)` in `investigation::extract_zip_text`) is a prerequisite
  for the v1 chat feature to support .zip attachments.

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

### 6.2 Keybindings inside the chat tab (V1 ship list)

The mockup above shows the eventual UX; v1 ships a trimmed keybinding
set. Polish lands in v2.

| Key | Action | V1? |
|---|---|---|
| `Ctrl-S` | Send turn (or `Enter` for single-line input) | yes |
| `Ctrl-F` | Attach file via **plain path prompt** (no picker — type or paste a path) | yes |
| `Ctrl-V` | Paste evidence — single text input, format `label=body` | yes |
| `Ctrl-R` | `/revise` — re-run the structured pipeline | yes |
| `Ctrl-T` | Retry the last codex call | yes |
| `Ctrl-C` / `Esc` | Cancel in-flight call or close the modal | yes |
| `q` | (modal closed) return to the inbox list | yes |
| `Ctrl-E` | Open `$EDITOR` (suspend TUI, edit, return) | **v2** |
| In-TUI file picker (arrow-keys + `/` filter) | replaces the path prompt | **v2** |
| Multi-line `Shift+Enter` editing | adds true multi-line input | **v2** |
| Animated gradient spinner | replaces the static throbber | **v2** |

### 6.3 New Cargo dependencies

| Crate | Version | Purpose | V1? |
|---|---|---|---|
| `tui-textarea` | `0.7` | Multiline editable widget with Vim/Emacs key bindings | yes (single-line use in v1; multi-line surfaces in v2) |
| `fs2` | `0.4` | Cross-platform advisory file lock (`try_lock_exclusive`) for the per-ticket session lock | yes |
| `throbber-widgets-tui` | `0.7` | Animated gradient spinner | **v2** (deferred) |

V1 uses a hand-rolled static braille spinner (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` cycling at
80ms) inline — no extra crate. Existing `ratatui`, `crossterm`, `tokio`,
`indicatif`, `thiserror`, `serde`, `serde_json`, `chrono`, `sha2`, `zip`
cover everything else. `sha2` is the existing dependency used for the
new evidence-provenance `sha256` fields.

## 7. Dataflow

### 7.1 Normal turn

```
1. Analyst presses [a] on ticket 44776 in inbox.
2. ChatPane reads:
     Tickets/44776/CONVERSATION.jsonl  -> Vec<Turn> (skips torn final line)
     Tickets/44776/.session/manifest.json -> provider posture check
     Tickets/44776/.session/base-ticket.json -> presence check only
   Provider posture (graceful degradation, section 8):
     - manifest.provider == LLM_PROVIDER -> try native resume
     - manifest.provider != LLM_PROVIDER -> replay under active provider
       with a yellow "session created under <other>; replaying" banner
     - no manifest -> first follow-up under current provider; create one
   ChatPane re-renders CONVERSATION.md from the JSONL if .md is stale
   (mtime older than .jsonl mtime) or missing.
3. Analyst types, attaches files (plain path prompt in v1), hits Ctrl-S.
4. ChatPane sends ChatEvent::SubmitTurn { body, evidence } to a tokio
   task. The chat task:
     a. Acquires the per-ticket advisory lock at .session/lock
        (fs2::try_lock_exclusive, 5s budget). On contention: yellow
        banner, retry, fail with hard error if exceeded.
     b. Reads the last turn number from CONVERSATION.jsonl tail;
        assigns next_turn = last + 1.
     c. For each evidence file: computes sha256, copies into
        Tickets/44776/attachments/turn-NNN/, builds the provenance
        record (section 5.3). Sources already under Tickets/44776/ are
        referenced by relative path, not copied; provenance records the
        original copied_path as the same path.
     d. Appends the analyst turn JSON object to CONVERSATION.jsonl as
        one line, fsync'd before lock release.
     e. Re-renders CONVERSATION.md from the full JSONL atomically.
     f. Releases the lock.
     g. Calls pipeline::followup_turn(ticket_id, manifest, prompt,
        evidence, &provider). This function:
          - reads session_id from manifest if provider matches
          - applies PII redaction to prompt + paste bodies + extracted
            file text (existing redact.rs scope; section 9.3 covers the
            disk-persistence side)
          - calls provider.followup(session_id, prompt, sys, model, atts)
          - returns FollowupResult (text + session_id + resumed flag)
     h. Streams elapsed-time updates through a tokio channel
        (existing ChannelReporter pattern) so the static throbber
        updates with real elapsed counts.
     i. On success: re-acquires lock, appends the codex turn to
        CONVERSATION.jsonl, re-renders .md, updates .session/manifest
        (bump resume_count, set last_resumed_at), releases lock.
     j. On failure: re-acquires lock, appends a system turn noting the
        failure with the provider error text; offers /retry.
5. ChatPane reads the new Turn struct from the channel and re-renders
   without re-parsing the whole file.
```

### 7.2 Revise turn

```
1. Analyst types /revise (or Ctrl-R) in the input modal.
2. Chat task validates:
     a. Is there at least one new analyst-or-automated turn since the
        last system turn with action=revise (or since turn-001 if
        none)? Required.
     b. Among those turns, does at least one carry NEW evidence — a
        file or labeled paste? A new question-only turn does NOT
        qualify on its own. Required.
   If either check fails: refuses with a yellow system turn explaining
   which gate tripped. The five-markdown folder is NOT touched.
3. Chat task loads the durable snapshots:
     - .session/base-ticket.json -> models::Ticket (serde_json round-trip)
     - .session/base-evidence-manifest.json -> Vec<EvidenceItem>
   If either snapshot is missing or fails to deserialize:
     - Try a live Zendesk re-fetch of the ticket; if that succeeds,
       proceed with a soft-warn "base snapshot missing; re-fetched"
       in STATE.md.validator_warnings.
     - If the live fetch also fails (e.g. offline), abort the revise
       with a red system turn and a clear "this ticket has no durable
       snapshot; reopen with network access" message. The structured
       folder is NOT rewritten.
4. Chat task builds a synthetic TriageBundle:
     - base Ticket from the snapshot
     - base evidence from the snapshot, with original IDs (E-001, ...)
       preserved verbatim
     - NEW evidence (files + pastes) from CONVERSATION.jsonl turns
       since the last revise added as additional EvidenceItems with
       freshly-assigned IDs (E-NNN starting at last_id+1)
     - prior codex turn bodies are NOT re-fed; the structured pipeline
       gets the structured-output prompt with evidence, not chat
       history. Analyst-turn body text is fed as PastedEvidence so it
       counts as "what the analyst told us" alongside files.
5. Chat task acquires the per-ticket lock, calls
   pipeline::investigate_one_structured(bundle, rubric,
   followup_mode=true). The followup_mode flag tells the pipeline to:
     - skip Zendesk re-fetch (the bundle already carries the Ticket)
     - preserve CONVERSATION.jsonl and CONVERSATION.md (writes are
       additive only; the five-markdown folder is the only thing
       rewritten)
     - record a "revised from turns X..Y" entry in
       STATE.md.validator_warnings
6. The five-markdown folder is rewritten atomically (same existing
   STATE.md soft-lock path). On STATE.md soft-lock conflict
   (different owner, no --force): write a system turn with the
   existing-owner / new-owner diff; offer [f] to retry with --force,
   [Esc] to cancel. The five-markdown folder remains unchanged on
   conflict.
7. Chat task appends a system turn (action=revise, outcome=success/
   conflict/validation_failed) to CONVERSATION.jsonl with the field-
   level diff. Releases the lock. Re-renders CONVERSATION.md.
8. Inbox row 44776 reads STATE.md when the chat pane closes; row
   updates to show the new fork letter with a small "revised" marker.
```

### 7.3 First-time chat-pane open on a ticket created BEFORE this feature

A ticket whose `Tickets/<id>/` was written before v1 of the chat
feature shipped will not have `.session/base-ticket.json` or
`.session/base-evidence-manifest.json`. The chat pane handles this:

1. On open, if `.session/base-ticket.json` is missing, the pane shows
   a yellow banner "this investigation predates the chat feature;
   re-fetching the ticket from Zendesk to populate the snapshot."
2. It calls the existing Zendesk client to fetch the ticket fresh and
   writes `.session/base-ticket.json` atomically.
3. For `.session/base-evidence-manifest.json`, it parses the existing
   `EVIDENCE_PREFLIGHT.md` to reconstruct the EvidenceItem list, then
   writes the manifest. This parse is best-effort and tolerant — fields
   that fail to parse are recorded with `summary: "<original markdown
   row>"` rather than blocking the chat open.
4. From that point forward the snapshot is authoritative; we never
   re-parse `EVIDENCE_PREFLIGHT.md`.

This is the only place the spec parses a markdown contract file, and
it's gated to the migration / first-open path. The steady state is
JSON snapshots, not markdown parsing.

## 8. Error handling

| Failure mode | Detection | Response |
|---|---|---|
| Codex session expired / unrecoverable | `codex exec resume` non-zero with the contract-gated "session not found" signal (section 5.6) | Fall back to replay-context under the same provider; log yellow system turn "session lost, replayed N turns"; CONVERSATION.jsonl preserved; new session ID captured on next successful call |
| Provider mismatch (manifest.provider ≠ LLM_PROVIDER) | Read at chat pane open | **Graceful degradation:** yellow banner "session was created under <other>; replaying under <current>"; the turn goes through using the active provider's replay-context path. Native resume is only attempted when the providers match. Hard-refuse only when the analyst opts in via `chat.require-native-resume = true` in `.env` or a `--require-native-resume` flag. |
| Soft-lock conflict on `/revise` | `TicketFolderError::SoftLockConflict` | System turn with existing-owner / new-owner diff; `[f]` to retry with `--force`, `[Esc]` to cancel. Five-markdown folder remains unchanged. |
| Structured-output validation failure after `/revise` | `LlmError::StructuredAfterRetry` | System turn with raw-response stash path; CONVERSATION.jsonl preserved; five-markdown folder NOT rewritten |
| Per-ticket lock contention | `fs2::try_lock_exclusive` returns `WouldBlock` | Yellow banner "another writer holds the lock; retrying"; backoff up to 5s; then hard error with a red system turn naming the lock-holder (manifest writes record `last_locked_by` for diagnosis) |
| File attach > 1 MB (raw on-disk bytes, before any extraction) | `chat::attach_file` size check via `fs::metadata` | Soft-warn yellow turn; `[f]` to force or `[t]` to truncate extracted text to first 256 KB (UTF-8 boundary safe). Binary / zip files: force-or-skip only. |
| In-flight codex call > 120s | tokio timer | Soft-warn at 60s; red at 120s with `[c]` to cancel (SIGTERM to codex subprocess); system turn recording the cancel |
| `codex` binary not on PATH | `which::which("codex").is_err()` (same as existing codex provider check) | Red banner with install hint; chat pane refuses to accept turns until codex is available or LLM_PROVIDER is changed |
| Torn final line in CONVERSATION.jsonl | JSON parse failure on the last line | Skip the torn line silently; surface a yellow `⚠ recovered from torn final write` indicator in the session sidebar so the analyst knows recovery happened |
| Base snapshot missing on `/revise` | `.session/base-ticket.json` or `.session/base-evidence-manifest.json` absent or unparseable | Live Zendesk re-fetch with soft-warn; if re-fetch fails, abort revise with red system turn — five-markdown folder unchanged |

A new `PipelineError::Followup` variant carries the new failure-mode
classes that don't already fit `Zendesk` / `Datadog` / `Llm` / `Extract`
/ `Memory` / `TicketFolder`. Variants: `SessionLostNoReplay`,
`CodexSessionCaptureFailed`, `LockContention`, `BaseSnapshotMissing`.

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

- `Tickets/<id>/CONVERSATION.jsonl` — versioned per-line via
  `"schema_version": 1`. Future schema changes carry a new
  schema_version on new lines; the parser handles mixed versions
  gracefully (unknown versions are read as opaque + ignored for
  prompt construction).
- `Tickets/<id>/CONVERSATION.md` — derived artifact only, regenerated
  on every JSONL change. Not a contract surface; safe to delete (will
  be regenerated on next chat-pane open).
- `Tickets/<id>/.session/` — version-tagged via `manifest.json.version`
  and `base-evidence-manifest.json.schema_version`. The `lock` file is
  ephemeral.
- `Tickets/<id>/attachments/turn-NNN/` — opaque per-turn attachment
  directory; no schema beyond filesystem layout. Files inside are
  byte-identical copies of analyst-supplied sources at intake time.
- One new `STATE.md.validator_warnings` pattern: `"revised from turns X..Y"`.
  Backward compatible with the existing schema (validator_warnings is
  already a free-form string array).

### 9.3 PII and attachment retention

The existing `redact.rs` redactor protects the LLM boundary — it scrubs
caller PII (phones, addresses, GPS) from the prompt before it reaches
the provider. **It does not protect disk persistence.** This feature
introduces new disk-persisted attachment surfaces (`attachments/turn-NNN/`
and `.session/base-*.json`) that need their own retention story.

**V1 retention policy:**

- Attachments are copied to `Tickets/<id>/attachments/turn-NNN/` as
  byte-identical copies of the analyst source. **The redactor is not
  applied to the on-disk copy** — the disk copy reflects the original
  evidence so a future analyst can reproduce the investigation.
- The redactor IS applied to the in-memory prompt that goes to the
  provider (existing behavior, unchanged).
- `evidence.body` in pasted evidence is stored verbatim in CONVERSATION
  .jsonl — same model as attachments. Audit trail requires this.
- Operators concerned about disk-persisted PII can:
  - Set `TRIAGE_TICKETS_ROOT` to an encrypted-at-rest volume (filesystem
    -level encryption is operator's responsibility, not in scope here).
  - Run `triage-cli redact-tickets <id>` after close-out (v2 tool — not
    in v1 scope; tracked separately).
- The `evidence.sent_to_provider` provenance field records whether each
  evidence item actually reached the LLM. Disk copy is independent;
  this field documents the LLM boundary state.

**V1 explicit non-promise:** the chat feature does not auto-redact disk
copies. Tickets containing customer PII follow the same retention
posture as the existing five-markdown folder, which is also not
auto-redacted. If retention policy changes (e.g. mandated 90-day
purge), a separate cleanup tool addresses both surfaces together.

## 10. Testing

| Layer | What | How |
|---|---|---|
| **Codex contract gate (section 5.6)** | Session-ID capture method is stable and parseable across `codex exec` and `codex exec resume` | Standalone integration test that runs the **real** codex binary against a canned prompt in a tempdir, asserts the capture method's output shape, and records the result. Must pass before any other codex-followup work is considered ready. Skipped in CI if `codex` is unavailable; gated on a `CODEX_AVAILABLE=1` env var. |
| `chat::parse_conversation_jsonl` | JSONL round-trip: parse → serialize → byte-identical; torn-final-line recovery | Inline `#[cfg(test)]` in `chat.rs`. Cover every turn_kind, missing-optional-field defaults, and a deliberately-truncated final line. |
| `chat::render_conversation_md` | JSONL → markdown render is deterministic and idempotent | Snapshot test (`insta`); same JSONL produces same markdown bytes regardless of run. |
| `chat::slash_command` | Slash-command parsing including error cases (`/revise` without new evidence, `/file` with missing path, `/paste` with malformed `label=` prefix) | Inline tests; table-driven. |
| `chat::lock` | Lock contention behavior: acquire, contend, retry, give up | Tempdir-based; spawn two threads racing for the same lock; assert second sees `WouldBlock` then succeeds after first drops handle. |
| `chat::evidence_provenance` | sha256 stability, truncation semantics, sent_to_provider truth across redaction outcomes | Inline tests; fixture files of various types (.log, .json, .zip with the existing test fixtures, binary garbage). |
| `providers::codex::followup` | Session-resume happy path + session-lost fallback + provider-mismatch graceful degradation | Mock codex subprocess via a fixture script on PATH; test-only `MOCK_CODEX_PATH` env var honored under `#[cfg(test)]`. Whatever capture method the gate selects is what the mock simulates. |
| `pipeline::followup_turn` | Append-only behavior, attachment copy + sha256, session-id update, redactor invoked on prompt | Tempdir-based unit test with a fake `LlmProvider` impl. |
| `pipeline::investigate_one_structured` in followup_mode | Revise path: base-snapshot loaded, new evidence merged with preserved IDs, CONVERSATION.jsonl preserved, attachments folded into bundle, STATE.md.validator_warnings updated | Extend existing fixture-based golden-output test (roadmap #5) with a `with-followup-evidence` fixture. |
| `tui/chat` | Snapshot rendering of the pane with various turn-kind colors and the static throbber frame | Ratatui's `TestBackend` plus `insta`. Animated gradient is **not** tested in v1 (the gradient lives in v2). |
| Migration path (section 7.3) | Pre-feature ticket opened in chat pane reconstructs base snapshots correctly | Fixture: a `Tickets/<id>/` that has only the five-markdown files; open chat pane in a test harness; assert `.session/base-*.json` are written and the parsed evidence IDs match the EVIDENCE_PREFLIGHT.md rows. |

No network outside the codex-contract-gate test. All other
codex/unleash calls in tests go through the mock subprocess or a fake
`LlmProvider` impl.

## 11. Automation hooks (forward-looking — schema in v1, writers in v2)

The user has called out automated triage and ticket-fetching as a
near-term follow-on initiative. **V1 ships the schema and lock surface**
that future automation will use, but no automated writers ship in v1.
This is a deliberate "design now, build later" boundary so v2 work is
purely additive.

### 11.1 `turn_kind: automated` is a real schema value from day one (V1)

The parser, renderer, color palette, and CONVERSATION.jsonl schema all
include `automated` as a first-class variant in v1. The TUI renders
automated turns with the violet header. **No writer of automated turns
ships in v1** — the schema slot is reserved.

A future watcher that detects new Zendesk comments will call
`chat::append_automated_turn(ticket_id, body, evidence)` (a public
helper that v1 implements but v1 has no callers of). The chat pane
will render the turn when the analyst next opens the inbox. No code
path in the TUI needs to change to add a new automated source.

### 11.2 `pipeline::followup_turn` is a public library function (V1)

Not a TUI-only path. A future cron-style "auto-summarize new evidence"
task will call `pipeline::followup_turn` directly from `watcher.rs`
with `turn_kind: automated`. The chat pane is the consumer of the
result, not the only producer of work. V1 ships the function as
`pub`; v1 has no automated caller.

### 11.3 Slash commands route through a `ChatCommand` enum (V1)

Adding a future `/dispatch <jira-id>`, `/notify-team`, or
`/escalate-to-engineering` slash is a single variant addition, not a
TUI rewrite. The same enum is what an automated turn would emit if it
wanted to suggest an action.

V1 ships the enum with the v1 slash set (`File`, `Paste`, `Revise`,
`Retry`, `Quit`). New variants in v2 are additive.

### 11.4 Per-ticket lock is the concurrency primitive (V1)

The lock surface defined in section 5.5 is **not** TUI-only. Future
automated writers MUST acquire the same lock before appending a turn,
or they will race the analyst. V1 ships the lock; v2 automated writers
consume it. The lock contract:

- Acquire `Tickets/<id>/.session/lock` with `fs2::try_lock_exclusive`
- Read tail of CONVERSATION.jsonl to assign next turn number
- Append the turn line (fsync before drop)
- Re-render CONVERSATION.md
- Release the lock (handle drop)

Roadmap entries for the actual automated writers are tracked in
`docs/ROADMAP.md` as item #2 (Automation hooks for chat pane).

## 12. Open questions and prerequisite work

### 12.1 Codex contract gate (blocking)

Before the codex `followup` impl is implementation-ready, the contract
described in section 5.6 must be discharged. Specifically:

- Is `codex exec --json` available, and does it emit the session ID?
- If not, what is the exact stderr surface that carries the session ID,
  and is it stable across `codex exec` and `codex exec resume`?
- What is the exact "session not found" error surface, and is it stable
  enough to match on (or do we fall back to "any non-zero exit + replay"?)
- Are there codex CLI versions in the org's deployment that differ on
  these answers?

This is work item #0 in the implementation plan. Result: a tested
parser plus a recorded `codex_capture_method` choice in
`manifest.json`. If the gate fails, codex follows the unleash replay-
context path and the spec still ships — just without native session
continuity in v1.

### 12.2 EVIDENCE_PREFLIGHT.md migration parser (best-effort, v1)

For the section 7.3 first-time-open path, the chat pane reconstructs
`.session/base-evidence-manifest.json` from `EVIDENCE_PREFLIGHT.md`.
The exact parser shape (column order, row delimiters, soft-handling of
freeform analyst edits) is resolved at implementation time. The parser
is best-effort and tolerant: failed rows are recorded with their
original markdown text, not dropped. Reconstruction is one-shot per
ticket; the snapshot then becomes authoritative.

### 12.3 Other implementation-level details (non-blocking)

Exact `tui-textarea` keybinding map, exact static-throbber frame
timing, exact `fs2` retry backoff curve, exact log-formatting in
`CONVERSATION.md` for non-text attachments — all resolvable at
implementation time without re-opening the design. The writing-plans
skill will turn this spec into a step-by-step implementation plan and
address those details there.

## 13. Out of scope (deferred)

| Item | Why not in v1 |
|---|---|
| Custom in-TUI file picker | V1 uses a plain path prompt. Picker lands in v2 after the data model survives real tickets. |
| `$EDITOR` integration / `Ctrl-E` suspend-into-editor | Same as picker — v2 polish, not v1 ship. |
| Animated gradient spinner | V1 uses a static throbber. `throbber-widgets-tui` dependency lands in v2. |
| Automated turn writers (watcher / cron) | Schema slot for `automated` ships in v1; producers ship in v2. Tracked as ROADMAP item #2. |
| Image attachments to codex (`-i <path>`) | V1 sends text-only prompts. Image flow lands in v2 once we have a real use case. |
| Multi-line `Shift+Enter` input | V1 uses a single-line input or `$EDITOR` (also deferred). True multi-line lands in v2 with the picker. |
| Multi-analyst concurrent chat | Per-ticket lock makes single-writer correct; concurrent collaborative editing is a different problem with different trade-offs. |
| Streaming codex output (token-by-token render) | `codex exec` is request/response, not streaming. Streaming is a provider-level feature change. |
| Voice input or recorded audio attachments | Out of scope for the text-first v1. |
| Mobile / web access to the chat pane | The TUI is a terminal surface by design. |
| Auto-redaction of disk-persisted attachments | Existing five-markdown folder also stores customer PII verbatim; retention is operator-managed. If retention policy changes, a separate `triage-cli redact-tickets` tool addresses both surfaces together. |

---

**End of spec.**
