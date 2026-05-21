# Inbox TUI chat revamp — design spec

**Date:** 2026-05-20
**Status:** Approved (brainstorming pass; awaiting implementation plan)
**Predecessor:** `2026-05-17-interactive-investigation-design.md` (V1 chat surface that this spec extends)
**Roadmap item:** Refinement of `docs/ROADMAP.md` #7 (V1 already shipped); a partial down-payment on #5 (chat pane polish)

## Problem

The inbox TUI chat pane (`triage-cli-rs/src/tui/chat.rs` + `tui/inbox.rs::run_chat_session`) shipped as the V1 narrow loop of ROADMAP #7. When an analyst sends a query, the codex provider takes 5–30 s to respond; during that window the only feedback is a single thin status row near the bottom of the screen that says `⠹ codex is thinking… X.Xs elapsed (Esc to cancel)`. The analyst's eye is on the transcript, the signal is on a one-line bar they easily miss, the canned text never changes, and there are no structured phase boundaries — so the same "thinking…" string is shown whether the subprocess is starting, awaiting the model, or parsing JSONL output. There is no logging of chat interactions, so when a turn fails or hangs the only post-hoc evidence is whatever the analyst remembers. File attachment requires typing a literal path; pointing the chat at a directory is not supported.

This spec revamps the chat surface to fix all three: visible progress, durable logging, directory-aware attachment.

## Goals

1. **Visible progress.** While a call is in flight, a banner above the input box shows a braille spinner, the current pipeline stage, a stage-appropriate canned message, and elapsed time. The banner collapses gracefully on small terminals.
2. **Structured logging.** Every chat interaction (keys parsed into commands, evidence attaches, provider requests, provider responses, errors, cancels) appends one JSON Lines record to `<ticket_dir>/.session/chat-events.log` for ease of post-hoc debugging.
3. **Directory attachment.** Analysts can attach a single file (today's behavior) or point the chat at a directory; the chat collects matching files with sensible caps and reports what was picked up.

## Non-goals

- Streaming partial codex responses into the transcript. The codex subprocess emits its full response at the end; live streaming requires switching from `Command::output().await` to `spawn() + BufReader` over `stdout`. Deferred to a follow-on (the phase channel introduced here is the right shape to extend later).
- A custom ratatui file-picker UI. ROADMAP #7 V2 already calls this out.
- `$EDITOR` integration for multi-line composition.
- TachyonFX-style animated transitions. Tracked separately under ROADMAP #1.
- A global aggregated chat log across tickets. Per-ticket only for this release; cross-ticket aggregation can be derived later by walking `Tickets/*/.session/chat-events.log`.
- Logging outside the inbox TUI chat path (system-wide instrumentation is a larger effort).

## High-level architecture

The current control flow is:

```
inbox.rs::run_chat_session
    spawn task → send_analyst_turn → pipeline::followup_turn → provider.followup
    (poll JoinHandle::is_finished() every 80ms)
```

The revamp introduces a `tokio::mpsc::UnboundedSender<ChatEvent>` that the spawned task posts to as it crosses real subprocess and pipeline boundaries. The inbox event loop drains that channel on each tick, drives the banner from the most recent event, and forwards every event to a `ChatLogger` that writes one JSONL record per event:

```
Ctrl-S in Ask mode
   │
   ▼
spawn task with (tx, ticket_dir):
   send_analyst_turn_with_progress
     │
     ├ tx.send(AnalystAppended)
     ├ tx.send(Phase{Ingesting})
     ├ pipeline::followup_turn(... reporter=&ChatPhaseReporter::new(tx))
     │     ├ reporter.phase(ContextAssembled)
     │     ├ reporter.phase(SessionResumeAttempt)   ← codex-only branch
     │     ├ reporter.phase(ProviderAwait)
     │     ├ reporter.phase(ResponseParsed)
     │     └ reporter.phase(Saved)
     └ tx.send(TurnPersisted | ProviderError | Cancelled)
   │
   ▼
inbox event loop:
   while let Ok(evt) = rx.try_recv():
      logger.log(&evt);
      progress.update_from(&evt);
   redraw(progress)
```

Mpsc replaces `JoinHandle::is_finished()` polling as the lifecycle signal. The join handle is still owned (still aborted on Esc, dropped at function exit), but its *terminal* state is no longer how we know the call is done — the channel is.

**Channel lifetime.** One mpsc channel lives for the entire `run_chat_session` call (i.e. the duration the chat tab is open). The sender is cloned into each spawned per-turn task; the receiver is drained by the main event loop on every 80ms tick. This is what lets `SessionOpened` / `SessionClosed` / `KeyCommand` events (which fire from the main loop, not from any spawned task) share the same stream as the per-turn `Phase` events.

## Module changes

```
triage-cli-rs/src/
├── chat.rs                ← extend: ChatEvent / ChatStage / ChatPhaseReporter / ChatLogger,
│                            collect_dir_attachments, paths::chat_events_log
├── tui/chat.rs            ← refactor: replace InFlightState with ChatProgress;
│                            add render_phase_banner with responsive layout
├── tui/inbox.rs           ← refactor: run_chat_session uses mpsc<ChatEvent> instead of
│                            polling active_job.is_finished(); add Ctrl-D dir attach modal
│                            and /dir slash command
└── pipeline.rs            ← extend: followup_turn takes an Option<&dyn ChatPhaseReporter>
                              and emits a phase event at each real boundary
```

## Data shapes

All shapes live in `chat.rs` so that downstream consumers (UI, logger) depend on the same module.

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatStage {
    Ingesting,            // canned: "loading attachments…"
    ContextAssembled,     // canned: "reading the ticket…"
    SessionResumeAttempt, // canned: "resuming session…"
    ProviderAwait,        // canned: "asking around…" / "thinking it through…" (rotates)
    ResponseParsed,       // canned: "writing it up…"
    Saved,                // canned: "saved"
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionCloseReason { UserQuit, EscFromAsk, EscFromInflight, CtrlC, ProviderUnavailable }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CancelSource { EscKey, CtrlC, AppExit }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatEvent {
    // Lifecycle
    SessionOpened    { ticket_id: String, ts: DateTime<Utc> },
    SessionClosed    { ts: DateTime<Utc>, reason: SessionCloseReason },
    // Input intake (one per parsed input action; not one per keypress — that would flood)
    KeyCommand       { ts: DateTime<Utc>, command: String },     // "send", "/file", "/dir", "/paste", "/revise", "/retry", "/quit"
    EvidenceAttached { ts: DateTime<Utc>, provenance: EvidenceProvenance },
    EvidenceRejected { ts: DateTime<Utc>, reason: String },
    // In-flight turn
    AnalystAppended  { ts: DateTime<Utc>, turn: u32 },
    Phase            { ts: DateTime<Utc>, stage: ChatStage, elapsed_s: f64 },
    ProviderRequest  { ts: DateTime<Utc>, provider: String, model: String,
                       prompt_bytes: usize, attachments: usize, session_id: Option<String> },
    ProviderResponse { ts: DateTime<Utc>, elapsed_s: f64,
                       tokens_in: Option<u32>, tokens_out: Option<u32>,
                       resumed: bool, session_id: Option<String> },
    ProviderError    { ts: DateTime<Utc>, kind: String, message: String },
    TurnPersisted    { ts: DateTime<Utc>, codex_turn: u32 },
    Cancelled        { ts: DateTime<Utc>, by: CancelSource },
}

#[derive(Debug, Clone)]
pub struct ChatProgress {
    pub stage:       ChatStage,
    pub canned_msg:  &'static str,
    pub elapsed_s:   f64,
    pub frame_idx:   usize,
    pub resumed:     Option<bool>,
    pub session_id:  Option<String>,
}
```

### `ChatPhaseReporter` (the pipeline-facing surface)

```rust
pub trait ChatPhaseReporter: Send + Sync {
    fn phase(&self, stage: ChatStage);
}

pub struct MpscPhaseReporter {
    tx: tokio::sync::mpsc::UnboundedSender<ChatEvent>,
    started: std::time::Instant,
}

impl ChatPhaseReporter for MpscPhaseReporter {
    fn phase(&self, stage: ChatStage) {
        let _ = self.tx.send(ChatEvent::Phase {
            ts: Utc::now(),
            stage,
            elapsed_s: self.started.elapsed().as_secs_f64(),
        });
    }
}
```

This mirrors the existing `Reporter` trait pattern in `pipeline.rs` (`StderrReporter`, `ChannelReporter`, `MetricsReporter`); a chat-scoped reporter is the natural extension. `pipeline::followup_turn` accepts `Option<&dyn ChatPhaseReporter>` and calls `phase()` at five spots; existing callers that pass `None` are unaffected.

### `ChatLogger`

```rust
pub struct ChatLogger { writer: BufWriter<fs::File> }

impl ChatLogger {
    pub fn open(ticket_dir: &Path) -> Result<Self, ChatError> { /* … */ }
    pub fn log(&mut self, evt: &ChatEvent) {
        if let Ok(line) = serde_json::to_string(evt) {
            let _ = writeln!(self.writer, "{line}");
            let _ = self.writer.flush();
        }
    }
}

pub fn chat_events_log_path(ticket_dir: &Path) -> PathBuf {
    session_dir(ticket_dir).join("chat-events.log")
}
```

Per-event flush so a killed process leaves a usable tail (same rationale as `append_turn`'s `sync_all`).

## Phase boundaries in `pipeline::followup_turn`

The five phase events fire at these exact code positions (relative to the current `pipeline.rs:954`):

| Stage                  | Position                                                       |
|------------------------|----------------------------------------------------------------|
| `Ingesting`            | Posted by the caller before `followup_turn` is entered.        |
| `ContextAssembled`     | After `build_ticket_context_preamble` + replay assembly (~L 1042). |
| `SessionResumeAttempt` | Just before `provider.followup(...)` IF `last_codex_session.is_some()`. Skipped for unleash. |
| `ProviderAwait`        | Just before the `.await` on `provider.followup(...)` (always). |
| `ResponseParsed`       | After `result` returns and we know `result.resumed` (~L 1056). |
| `Saved`                | After `write_conversation_md` succeeds (~L 1124).              |

If the call fails, no `Saved` event fires — instead the caller emits `ProviderError` from the error branch in `send_analyst_turn_with_progress`. If the user cancels via Esc, a `Cancelled` event is emitted from a `Drop` guard on the spawned task scope (so abort paths still log).

The pipeline reporter is **optional**. Existing CLI callers (`triage`, `investigate`, `watch`, `pipeline::revise`) continue to pass `None` and see no behavior change. Only the inbox chat path passes a real reporter.

## Canned-message mapping & rotation

```rust
fn canned_message(stage: ChatStage, rotation_idx: usize) -> &'static str {
    match stage {
        ChatStage::Ingesting            => "loading attachments…",
        ChatStage::ContextAssembled     => "reading the ticket…",
        ChatStage::SessionResumeAttempt => "resuming session…",
        ChatStage::ProviderAwait => {
            const AWAIT_CYCLE: [&str; 4] = [
                "asking around…",
                "thinking it through…",
                "still working…",
                "the model is taking its time…",
            ];
            AWAIT_CYCLE[rotation_idx % AWAIT_CYCLE.len()]
        }
        ChatStage::ResponseParsed       => "writing it up…",
        ChatStage::Saved                => "saved",
    }
}
```

The rotation index is `(elapsed_s / 4.0) as usize` (overall elapsed since turn start), so the `ProviderAwait` text changes every ~4 seconds. The underlying `ChatStage` does *not* change while in `ProviderAwait`, so the log records exactly one Phase event for the await, not one per rotation. Rotation is a pure UI cosmetic.

## Progress banner — responsive layout

Layout is computed every draw from `area.height`. There are four tiers; the same `ChatProgress` data drives all of them:

```rust
fn banner_rows(area_height: u16) -> u16 {
    match area_height {
        h if h >= 20 => 4,   // full banner: bordered, title, spinner+stage+elapsed,
                             //              session badge, hint row
        h if h >= 14 => 3,   // bordered, single content row, hint row
        h if h >= 10 => 2,   // unbordered separator + single content row
        _            => 1,   // single-line status (current behavior)
    }
}
```

**Full (≥20 rows):**

```
┌─ codex follow-up ──────────────────────────────────────────────┐
│ ⠹  asking around…  stage: provider_await   elapsed 2.3s        │
│    session: resumed (sid 01HFAKE12345)                         │
│    Esc cancels · Ctrl-T retries last turn                      │
└────────────────────────────────────────────────────────────────┘
```

**Compact (14–19 rows):**

```
┌─ codex follow-up ──────────────────────────────────────────────┐
│ ⠹  asking around…  2.3s  (Esc cancel)                          │
└────────────────────────────────────────────────────────────────┘
```

**Tight (10–13 rows):**

```
─── ⠹  asking around…  2.3s  (Esc cancel) ──────────────────────
```

**Single-row (<10 rows):** unchanged from today's status line.

**Color by stage** (existing constants in `tui/chat.rs`):
- `Ingesting` / `ContextAssembled` → `SYSTEM_HEADER` (orange) — "getting ready"
- `SessionResumeAttempt` / `ProviderAwait` → `CODEX_HEADER` (green) — "talking to the model"
- `ResponseParsed` / `Saved` → `ANALYST_HEADER` (blue) — "wrapping up"

The banner is rendered only when `progress.is_some()`. When no call is in flight, the layout returns those rows to the input box (input grows from 5 → 5+banner_rows lines), so screen real estate isn't wasted.

**Resize handling.** Layout is recomputed every draw. A crossterm `Resize` event becomes invisible to the chat code: the next 80ms tick draws with `f.area()`, which already has the new dimensions, and `banner_rows` clamps appropriately. No state to recover, no special branch.

## Directory attachment

### New slash command and keybinding

```rust
pub enum ChatCommand {
    File(PathBuf),
    Dir { path: PathBuf, recursive: bool, glob: Option<String> },  // new
    Paste { label: String, body: String },
    Revise, Retry, Quit,
    Body(String),
}
```

Parser additions:
- `/dir <path>` → `Dir { path, recursive: false, glob: None }`
- `/dir <path> -r` → `Dir { path, recursive: true, glob: None }`
- `/dir <path> *.log` → `Dir { path, recursive: false, glob: Some("*.log") }`
- `/dir <path> -r *.log` → `Dir { path, recursive: true, glob: Some("*.log") }`

A new `Ctrl-D` keybinding opens a `ChatInputMode::DirPath(String)` modal analogous to the existing `FilePath` modal. The modal accepts the same syntax as the slash command.

### `chat::collect_dir_attachments`

```rust
pub struct DirCollectResult {
    pub attached: Vec<EvidenceProvenance>,
    pub skipped:  Vec<DirSkipped>,
}

pub enum DirSkipped {
    FileCapExceeded { path: PathBuf },
    SizeCapExceeded { path: PathBuf, bytes: u64 },
    UnsupportedType { path: PathBuf },
    GlobMismatch    { path: PathBuf },
}

pub fn collect_dir_attachments(
    ticket_dir: &Path,
    turn_no: u32,
    dir: &Path,
    recursive: bool,
    glob: Option<&str>,
    cap_files: usize,        // default 25
    cap_total_bytes: u64,    // default 4 MiB
) -> Result<DirCollectResult, ChatError>
```

Behavior:
- Walks `dir` (one level deep by default; recursive on `recursive: true`).
- Filters by glob if supplied, otherwise by `investigation::detect_file_type` returning anything except `FileType::Unknown`, AND by an extension allow-list: `txt log md json csv yaml yml conf ini rs py ts tsx js jsx`.
- Sorts deterministically: `(parent_path, basename)` so test output is stable.
- Calls existing `chat::attach_file` for each accepted file (so sha256, copy-into-ticket, and provenance work identically).
- Stops adding files when `cap_files` or `cap_total_bytes` would be exceeded; remainder goes to `skipped` with the appropriate reason.

After the call, the run_chat_session loop:
1. Extends `pending_evidence` with `result.attached`.
2. Appends a `System` turn summarizing the batch: `"attached 7 file(s) from ./logs/ (skipped 2 over size cap, 1 unsupported type)"`. The system turn's body includes a bulleted list of skipped paths so the analyst can decide whether to widen the scope.
3. Logs one `EvidenceAttached` event per accepted file and one `EvidenceRejected` event per skipped file.

## Event-loop refactor

The current `run_chat_session` polls `active_job.is_finished()` on each tick to decide whether to clear `in_flight`. The refactor replaces that with mpsc consumption:

```rust
let (tx, mut rx) = mpsc::unbounded_channel::<ChatEvent>();
let mut logger = chat::ChatLogger::open(&ticket_dir)?;
let mut progress: Option<ChatProgress> = None;
let mut active_job: Option<tokio::task::JoinHandle<Result<(), String>>> = None;

loop {
    // Drain pending events (non-blocking).
    while let Ok(evt) = rx.try_recv() {
        logger.log(&evt);
        progress = update_progress(progress.take(), &evt);
        if matches!(evt, ChatEvent::TurnPersisted { .. }
                       | ChatEvent::ProviderError { .. }
                       | ChatEvent::Cancelled { .. }) {
            active_job = None;     // task is reporting completion
            progress = None;       // banner clears
        }
    }

    // Advance spinner frame_idx based on wall clock, not tick count.
    if let Some(p) = progress.as_mut() {
        p.frame_idx = ((p.elapsed_s * 12.5) as usize) % THROBBER_FRAMES.len();
        p.canned_msg = canned_message(p.stage, (p.elapsed_s / 4.0) as usize);
    }

    // Draw with current progress, then poll keys.
    terminal.draw(|f| f.render_widget(&pane(progress.as_ref(), ...), f.area()))?;
    if let Ok(Some(key)) = poll_key_event(Duration::from_millis(80)).await {
        handle_key(key, &mut input_mode, &mut active_job, &tx, ...);
    }
}
```

`update_progress` is a small pure function with a state machine; tests against it are cheap and direct.

### Cancel discipline

When the user presses Esc while a call is in flight, the inbox loop:
1. Calls `handle.abort()` on the join handle.
2. Sends a `ChatEvent::Cancelled { by: CancelSource::EscKey }` directly through `tx` (the aborted task may not get a chance to drop its guard cleanly).
3. Sets `active_job = None`, `progress = None`.
4. Sets `status_hint = Some("turn cancelled")`.

The aborted task's `Drop` guard is also present as a safety net — if the abort happens cleanly, the guard fires; if it doesn't, the inbox-emitted event covers it. Worst case is two `Cancelled` events for one turn, which the log can dedupe by `ts` if needed — not a correctness problem.

## PII boundary

The chat log MUST NOT contain:
- Prompt body text (only `prompt_bytes` byte count).
- Evidence body content (only `EvidenceProvenance`, which is sha256, paths, bytes, label — no body).
- System prompt content.
- Codex stdout/stderr — only the extracted top-level error message in `ProviderError.message`, and only after the existing redaction at the LLM boundary.

The existing `redact::redact` boundary continues to be the canonical place where caller PII is scrubbed. The chat logger sits *inside* the redaction perimeter — it only sees data after redaction has already been applied. As a defense-in-depth measure, the `ProviderError.message` field is also redacted by `redact::redact` before being logged, since subprocess error text occasionally echoes back fragments of the prompt.

## Tests

All tests are inline `#[cfg(test)]` per repo convention. No new network I/O.

### `chat.rs`

- `ChatLogger` round-trip: write 6 events of mixed kinds, read back the JSONL, deserialize each, assert tags + payloads match.
- `ChatLogger` survives I/O failure: when the underlying file is `chmod 000`'d, `log()` returns silently (no panic, no error propagation).
- `collect_dir_attachments`:
  - Hard cap (`cap_files`): more files than the cap → first N attached, remainder skipped with `FileCapExceeded`.
  - Hard cap (`cap_total_bytes`): aggregate-size cap, applied in walk order; remainder gets `SizeCapExceeded`.
  - Glob filter: only matching files attached, rest get `GlobMismatch`.
  - Recursive vs single-level: a nested file is skipped when `recursive: false`, included when `true`.
  - Type filter: a `.bin` file gets `UnsupportedType` unless an explicit glob matches it.
- `canned_message`: every `ChatStage` returns a non-empty string; `ProviderAwait` cycles through 4 distinct strings; rotation_idx wraps via modulo without panicking.
- `update_progress` state machine: feeding the canonical event sequence (`AnalystAppended → Phase(Ingesting) → Phase(ContextAssembled) → Phase(ProviderAwait) → ProviderResponse → Phase(Saved) → TurnPersisted`) yields the expected `Option<ChatProgress>` after each step.

### `tui/chat.rs`

- Snapshot rendering at four heights (8, 12, 18, 28 rows): banner collapses through all four tiers; no buffer overrun, no clipped block borders, no panic.
- Snapshot rendering with `progress: None`: banner is absent, input box gets the freed rows.
- Snapshot rendering with each `ChatStage`: correct color + correct canned text appear on the banner row.
- Spinner advances purely from `elapsed_s` (not from tick count): two calls with identical `elapsed_s` produce identical `frame_idx`.

### `pipeline.rs`

- `followup_turn` with a recording `ChatPhaseReporter`: in the codex-resume happy path (mock provider), exactly five phase events fire in order: `ContextAssembled → SessionResumeAttempt → ProviderAwait → ResponseParsed → Saved`.
- Same call with no prior session: `SessionResumeAttempt` is skipped; four phases fire.
- Existing `followup_turn` callers (passing `None` as reporter) still pass their current assertions — no regression.
- Provider error after `ProviderAwait`: `ResponseParsed` and `Saved` do NOT fire; the caller is responsible for emitting `ProviderError`.

### `tui/inbox.rs`

- `run_chat_session` event-loop integration: drive a fake provider that takes 200ms; assert the channel receives all five phase events and the `TurnPersisted` event in order; assert the log file contains those events serialized as JSONL; assert `progress` is `None` after `TurnPersisted`.
- Cancel path: spawn a fake provider that sleeps 5s; after 100ms send Esc; assert `Cancelled` is logged exactly once (or exactly twice with identical `ts` — both acceptable, deduped by the log reader if needed).
- `/dir` with caps: drive a directory with 30 files; assert exactly 25 are attached, 5 are logged as `EvidenceRejected{FileCapExceeded}`, and one `System` turn summarizing the batch is appended.

## Migration & compatibility

- `InFlightState` (the current `pub struct`) is replaced by `ChatProgress`. Direct callers are only `tui/chat.rs::ChatPane` and `tui/inbox.rs::run_chat_session`; both are in-crate. Public API outside the crate is unaffected.
- Existing tickets have no `chat-events.log`; the file is created lazily on the first chat event after the upgrade. No migration script.
- The existing `CONVERSATION.jsonl` schema is **unchanged**. Phase events live in the new sidecar log; conversation turns continue to be the durable record of the dialog.
- `pipeline::followup_turn` gains an optional reporter parameter (signature change). Internal-only — call sites are in this crate. Library users outside the crate are not in scope.

## Out of scope (deliberate)

- Streaming partial codex output into the transcript. The phase channel is structured to support this later: a `ChatEvent::PartialResponse { fragment: String }` variant can be added without breaking existing consumers.
- File-picker UI, `$EDITOR` integration, animated gradients, multi-line `Shift+Enter` — all tracked under ROADMAP #1 V2 / #5.
- Cross-ticket aggregated logging. The per-ticket JSONL is grep-friendly across the whole inbox via `find Tickets -name chat-events.log -exec cat {} +` if needed.
- Logging on the non-chat paths (`investigate`, `triage`, `watch`). Larger system-wide effort, not in scope for this revamp.

## Estimated PR shape

- `chat.rs`: ~250 lines (ChatEvent + ChatStage + ChatLogger + ChatPhaseReporter + collect_dir_attachments + canned_message + tests)
- `tui/chat.rs`: ~150 lines (responsive banner + ChatProgress + render_phase_banner + snapshot tests)
- `tui/inbox.rs`: ~200 lines (mpsc-driven event loop + Ctrl-D modal + /dir parser branch)
- `pipeline.rs`: ~50 lines (Optional<&dyn ChatPhaseReporter> + 5 emission sites + tests)

Total: **~650 lines including tests**. Larger than the original chat-pane V1 (#7) because logging + dir-walker + responsive layout are additive, but smaller than a streaming refactor (deferred).

## Open questions

None as of approval. Defaults committed:
- Dir-attach caps: 25 files, 4 MiB aggregate, single-level by default, `-r` opt-in.
- ChatStage boundaries: six total (Ingesting + the five pipeline-emitted ones: ContextAssembled, SessionResumeAttempt, ProviderAwait, ResponseParsed, Saved). Sub-millisecond stages (redaction, markdown render) folded into adjacent ones.
- Banner placement: above the input box, four responsive tiers down to a single-row fallback (<10 rows).
- Logging destination: per-ticket JSONL at `<ticket_dir>/.session/chat-events.log`. No cross-ticket aggregation.
