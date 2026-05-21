# Inbox TUI Chat Revamp Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the single-row throbber in the inbox chat pane with a responsive progress banner driven by a typed phase channel, add per-ticket JSONL logging for every chat interaction, and add directory-aware evidence attachment via `/dir` and `Ctrl-D`.

**Architecture:** A `tokio::sync::mpsc::UnboundedSender<ChatEvent>` channel is created once per `run_chat_session` call. The spawned per-turn task emits typed `ChatEvent` records (lifecycle, phase, evidence, provider request/response). The main event loop drains the channel each 80ms tick, forwards every event to a `ChatLogger` that writes JSONL to `<ticket_dir>/.session/chat-events.log`, and updates a `ChatProgress` struct that drives a responsive banner widget (4-row → 3-row → 2-row → 1-row tiers clamped by `area.height`). `pipeline::followup_turn` gains an optional `&dyn ChatPhaseReporter` parameter that emits five `Phase` events at real pipeline boundaries; existing CLI callers pass `None` and see no behavior change.

**Tech Stack:** Rust 1.95+, `tokio` (mpsc, sync), `serde` + `serde_json` (event serialization), `chrono` (timestamps), `ratatui` + `crossterm` (TUI), `fs2` (existing per-ticket lock). No new crate dependencies.

**Spec:** [docs/superpowers/specs/2026-05-20-inbox-chat-revamp-design.md](../specs/2026-05-20-inbox-chat-revamp-design.md)

---

## File map

```
triage-cli-rs/src/
├── chat.rs                ← extend: ChatStage, ChatEvent, ChatProgress, ChatLogger,
│                            ChatPhaseReporter trait + MpscPhaseReporter impl,
│                            collect_dir_attachments, chat_events_log_path,
│                            canned_message, update_progress, advance_progress_tick,
│                            simple_glob_match
├── tui/chat.rs            ← refactor: replace InFlightState with ChatProgress;
│                            extend ChatCommand with Dir variant + parser branch;
│                            add render_phase_banner + banner_rows
├── tui/inbox.rs           ← refactor: run_chat_session uses mpsc<ChatEvent> instead of
│                            polling JoinHandle::is_finished; add Ctrl-D modal +
│                            ChatInputMode::DirPath; wire ChatLogger; send_analyst_turn
│                            renamed → send_analyst_turn_with_progress
└── pipeline.rs            ← extend: followup_turn signature gains
                              Option<&dyn ChatPhaseReporter>; 5 emission sites
```

**Existing types touched but not renamed:** `EvidenceProvenance`, `Turn`, `TurnKind`, `Attachment`, `FileType`, `InvestigateOptions`. These keep their current shape — only references change in callers.

**Removed:** `InFlightState` (replaced by `ChatProgress` — the old struct's two fields `elapsed_s` and `frame_idx` become part of the new struct).

---

## Task 1: ChatStage enum + canned_message helper

**Files:**
- Modify: `triage-cli-rs/src/chat.rs` (append new code at end of module, before `#[cfg(test)] mod tests`)
- Test: `triage-cli-rs/src/chat.rs` (inline `#[cfg(test)]` tests)

- [ ] **Step 1: Write failing tests for ChatStage + canned_message**

Add to `triage-cli-rs/src/chat.rs` inside the existing `#[cfg(test)] mod tests` block (just before its closing `}`):

```rust
    // ── ChatStage + canned_message ──────────────────────────────────

    #[test]
    fn canned_message_is_non_empty_for_every_stage() {
        for stage in [
            ChatStage::Ingesting,
            ChatStage::ContextAssembled,
            ChatStage::SessionResumeAttempt,
            ChatStage::ProviderAwait,
            ChatStage::ResponseParsed,
            ChatStage::Saved,
        ] {
            for rot in 0..8 {
                let m = canned_message(stage, rot);
                assert!(!m.is_empty(), "stage {stage:?} rot {rot} returned empty");
            }
        }
    }

    #[test]
    fn canned_message_provider_await_rotates_four_distinct_strings() {
        let mut seen = std::collections::HashSet::new();
        for rot in 0..4 {
            seen.insert(canned_message(ChatStage::ProviderAwait, rot));
        }
        assert_eq!(seen.len(), 4, "expected 4 distinct rotations, got {seen:?}");
        // Modulo wraparound — rot=4 must equal rot=0.
        assert_eq!(
            canned_message(ChatStage::ProviderAwait, 0),
            canned_message(ChatStage::ProviderAwait, 4)
        );
    }

    #[test]
    fn canned_message_non_await_stages_ignore_rotation() {
        // Only ProviderAwait rotates. The other five stages return a constant.
        let m0 = canned_message(ChatStage::Ingesting, 0);
        let m9 = canned_message(ChatStage::Ingesting, 9);
        assert_eq!(m0, m9);
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cd triage-cli-rs && cargo test --lib chat:: 2>&1 | head -40`
Expected: FAIL — `cannot find type 'ChatStage' in this scope` / `cannot find function 'canned_message'`.

- [ ] **Step 3: Implement ChatStage enum + canned_message**

In `triage-cli-rs/src/chat.rs`, append (after the existing `build_conversation_replay` function, before `#[cfg(test)]`):

```rust
// ──────────────────────────────────────────────────────────────────────
//  Phase channel — ChatStage, canned messages
// ──────────────────────────────────────────────────────────────────────

/// Pipeline phase a chat turn is currently in. Each variant maps to a
/// stage-appropriate canned message via [`canned_message`]. The variant
/// values are stable wire-format strings (snake_case) used by [`ChatEvent`]
/// when serialized to the per-ticket chat-events.log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatStage {
    /// Caller is building the augmented prompt + attachments.
    Ingesting,
    /// `pipeline::followup_turn` has assembled the ticket-context preamble.
    ContextAssembled,
    /// Codex provider is about to try `codex exec resume`. Skipped for
    /// stateless providers (unleash) and for first turns with no prior session.
    SessionResumeAttempt,
    /// Provider call is in flight; awaiting model response.
    ProviderAwait,
    /// Provider returned; result has been parsed.
    ResponseParsed,
    /// Conversation JSONL + markdown have been written; turn is durable.
    Saved,
}

/// Map a `ChatStage` (and a UI-only rotation index used for the long
/// `ProviderAwait` phase) to the canned status string shown on the banner.
/// The rotation index has no effect on stages other than `ProviderAwait`.
pub fn canned_message(stage: ChatStage, rotation_idx: usize) -> &'static str {
    match stage {
        ChatStage::Ingesting => "loading attachments…",
        ChatStage::ContextAssembled => "reading the ticket…",
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
        ChatStage::ResponseParsed => "writing it up…",
        ChatStage::Saved => "saved",
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd triage-cli-rs && cargo test --lib chat::tests::canned_message 2>&1 | tail -20`
Expected: PASS — three tests pass.

- [ ] **Step 5: Commit**

```bash
git add triage-cli-rs/src/chat.rs
git commit -m "feat(chat): add ChatStage enum and canned_message helper

First slice of the inbox chat revamp (spec
2026-05-20-inbox-chat-revamp-design.md). Defines the six pipeline
phases a chat turn moves through plus the stage→canned-string map
that will drive the progress banner.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 2: ChatEvent enum with serde round-trip

**Files:**
- Modify: `triage-cli-rs/src/chat.rs` (append after Task 1's additions)
- Test: `triage-cli-rs/src/chat.rs` (inline)

- [ ] **Step 1: Write failing tests for ChatEvent serde**

Add to the `#[cfg(test)] mod tests` block in `chat.rs`:

```rust
    // ── ChatEvent ───────────────────────────────────────────────────

    fn now_ts() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap()
    }

    #[test]
    fn chat_event_session_opened_round_trips() {
        let evt = ChatEvent::SessionOpened {
            ticket_id: "44776".into(),
            ts: now_ts(),
        };
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"kind\":\"session_opened\""), "tag missing: {s}");
        assert!(s.contains("\"ticket_id\":\"44776\""), "field missing: {s}");
        let back: ChatEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(evt, back);
    }

    #[test]
    fn chat_event_phase_round_trips() {
        let evt = ChatEvent::Phase {
            ts: now_ts(),
            stage: ChatStage::ProviderAwait,
            elapsed_s: 2.3,
        };
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"kind\":\"phase\""));
        assert!(s.contains("\"stage\":\"provider_await\""));
        let back: ChatEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(evt, back);
    }

    #[test]
    fn chat_event_provider_request_round_trips() {
        let evt = ChatEvent::ProviderRequest {
            ts: now_ts(),
            provider: "codex".into(),
            model: "gpt-5.5".into(),
            prompt_bytes: 4096,
            attachments: 3,
            session_id: Some("01HFAKE12345".into()),
        };
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"kind\":\"provider_request\""));
        assert!(s.contains("\"prompt_bytes\":4096"));
        let back: ChatEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(evt, back);
    }

    #[test]
    fn chat_event_cancelled_round_trips() {
        let evt = ChatEvent::Cancelled {
            ts: now_ts(),
            by: CancelSource::EscKey,
        };
        let s = serde_json::to_string(&evt).unwrap();
        assert!(s.contains("\"kind\":\"cancelled\""));
        assert!(s.contains("\"by\":\"esc_key\""));
        let back: ChatEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(evt, back);
    }

    #[test]
    fn chat_event_all_variants_serializable() {
        // One smoke per variant so nobody adds a variant that breaks serde.
        let provenance = EvidenceProvenance::Paste {
            label: "note".into(),
            body: "body".into(),
            bytes: 4,
            sent_to_provider: true,
        };
        let events = vec![
            ChatEvent::SessionOpened { ticket_id: "1".into(), ts: now_ts() },
            ChatEvent::SessionClosed { ts: now_ts(), reason: SessionCloseReason::UserQuit },
            ChatEvent::KeyCommand { ts: now_ts(), command: "send".into() },
            ChatEvent::EvidenceAttached { ts: now_ts(), provenance: provenance.clone() },
            ChatEvent::EvidenceRejected { ts: now_ts(), reason: "too big".into() },
            ChatEvent::AnalystAppended { ts: now_ts(), turn: 7 },
            ChatEvent::Phase { ts: now_ts(), stage: ChatStage::Ingesting, elapsed_s: 0.0 },
            ChatEvent::ProviderRequest {
                ts: now_ts(), provider: "p".into(), model: "m".into(),
                prompt_bytes: 1, attachments: 0, session_id: None,
            },
            ChatEvent::ProviderResponse {
                ts: now_ts(), elapsed_s: 1.0, tokens_in: None, tokens_out: None,
                resumed: false, session_id: None,
            },
            ChatEvent::ProviderError { ts: now_ts(), kind: "io".into(), message: "x".into() },
            ChatEvent::TurnPersisted { ts: now_ts(), codex_turn: 8 },
            ChatEvent::Cancelled { ts: now_ts(), by: CancelSource::CtrlC },
        ];
        for evt in events {
            let s = serde_json::to_string(&evt).unwrap();
            let back: ChatEvent = serde_json::from_str(&s).unwrap();
            assert_eq!(evt, back, "round-trip failed for: {s}");
        }
    }
```

Also at the top of the test module, ensure `use chrono::TimeZone;` is added (other tests may already import chrono — check by reading the existing test imports first).

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cd triage-cli-rs && cargo test --lib chat::tests::chat_event 2>&1 | head -30`
Expected: FAIL — `cannot find type 'ChatEvent'`, `cannot find type 'SessionCloseReason'`, `cannot find type 'CancelSource'`.

- [ ] **Step 3: Implement ChatEvent + SessionCloseReason + CancelSource**

In `triage-cli-rs/src/chat.rs`, append after the `canned_message` function from Task 1:

```rust
/// Why a chat session ended. Recorded in `ChatEvent::SessionClosed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCloseReason {
    /// User typed `/quit` or pressed `q` in the inbox before re-entering chat.
    UserQuit,
    /// `Esc` from the `Ask` input mode with no call in flight.
    EscFromAsk,
    /// `Esc` while a provider call was in flight (cancels the turn AND the session).
    EscFromInflight,
    /// `Ctrl-C`.
    CtrlC,
    /// The configured LLM provider could not be instantiated at session start
    /// (e.g. codex binary missing, env var unset).
    ProviderUnavailable,
}

/// Source of a `ChatEvent::Cancelled` event. The cancel path may emit
/// from multiple sites (the inbox event loop and the spawned task's
/// Drop guard); the variant identifies which.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelSource {
    /// User pressed `Esc` while a call was in flight.
    EscKey,
    /// User pressed `Ctrl-C`.
    CtrlC,
    /// The session is being torn down (e.g. process exit, terminal closed).
    AppExit,
}

/// One record in the per-ticket chat-events log. Serialized to JSON Lines
/// at `<ticket_dir>/.session/chat-events.log`. The `kind` field is the
/// serde tag — readers can match on it without deserializing the payload.
///
/// PII boundary: payloads MUST NOT carry prompt body text, system prompt
/// content, evidence body content, or codex stdout. Only counts
/// (`prompt_bytes`) and redacted error messages. Caller PII redaction is
/// applied at the LLM boundary (`redact::redact`) before this event is
/// constructed.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatEvent {
    // ── Lifecycle ────────────────────────────────────────────────
    SessionOpened {
        ticket_id: String,
        ts: chrono::DateTime<chrono::Utc>,
    },
    SessionClosed {
        ts: chrono::DateTime<chrono::Utc>,
        reason: SessionCloseReason,
    },
    // ── Input intake (one per parsed action, NOT one per keypress) ──
    KeyCommand {
        ts: chrono::DateTime<chrono::Utc>,
        command: String,
    },
    EvidenceAttached {
        ts: chrono::DateTime<chrono::Utc>,
        provenance: EvidenceProvenance,
    },
    EvidenceRejected {
        ts: chrono::DateTime<chrono::Utc>,
        reason: String,
    },
    // ── In-flight turn ──────────────────────────────────────────
    AnalystAppended {
        ts: chrono::DateTime<chrono::Utc>,
        turn: u32,
    },
    Phase {
        ts: chrono::DateTime<chrono::Utc>,
        stage: ChatStage,
        elapsed_s: f64,
    },
    ProviderRequest {
        ts: chrono::DateTime<chrono::Utc>,
        provider: String,
        model: String,
        prompt_bytes: usize,
        attachments: usize,
        session_id: Option<String>,
    },
    ProviderResponse {
        ts: chrono::DateTime<chrono::Utc>,
        elapsed_s: f64,
        tokens_in: Option<u32>,
        tokens_out: Option<u32>,
        resumed: bool,
        session_id: Option<String>,
    },
    ProviderError {
        ts: chrono::DateTime<chrono::Utc>,
        kind: String,
        message: String,
    },
    TurnPersisted {
        ts: chrono::DateTime<chrono::Utc>,
        codex_turn: u32,
    },
    // ── Cancel ──────────────────────────────────────────────────
    Cancelled {
        ts: chrono::DateTime<chrono::Utc>,
        by: CancelSource,
    },
}
```

If `use chrono::TimeZone;` is missing from the test module, add it next to the existing `use chrono::Utc;`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd triage-cli-rs && cargo test --lib chat::tests::chat_event 2>&1 | tail -20`
Expected: PASS — five chat_event tests pass.

- [ ] **Step 5: Commit**

```bash
git add triage-cli-rs/src/chat.rs
git commit -m "feat(chat): add ChatEvent / SessionCloseReason / CancelSource enums

Adds the wire-format types for the per-ticket chat-events log.
Tagged JSON with snake_case discriminants so external tools (jq,
log readers) can match on the kind field without deserializing
the payload.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 3: ChatProgress struct + update_progress + advance_progress_tick

**Files:**
- Modify: `triage-cli-rs/src/chat.rs`
- Test: `triage-cli-rs/src/chat.rs` (inline)

- [ ] **Step 1: Write failing tests for the state machine**

Add to the `#[cfg(test)] mod tests` block:

```rust
    // ── ChatProgress state machine ──────────────────────────────────

    #[test]
    fn update_progress_canonical_sequence_advances_through_stages() {
        let mut p: Option<ChatProgress> = None;

        // 1. AnalystAppended — no banner yet, but session is in flight.
        p = update_progress(p, &ChatEvent::AnalystAppended { ts: now_ts(), turn: 7 });
        assert!(p.is_some(), "AnalystAppended should open progress");

        // 2. Phase(Ingesting)
        p = update_progress(
            p,
            &ChatEvent::Phase {
                ts: now_ts(),
                stage: ChatStage::Ingesting,
                elapsed_s: 0.1,
            },
        );
        assert_eq!(p.as_ref().unwrap().stage, ChatStage::Ingesting);
        assert!((p.as_ref().unwrap().elapsed_s - 0.1).abs() < 1e-6);

        // 3. Phase(ContextAssembled)
        p = update_progress(
            p,
            &ChatEvent::Phase {
                ts: now_ts(),
                stage: ChatStage::ContextAssembled,
                elapsed_s: 0.4,
            },
        );
        assert_eq!(p.as_ref().unwrap().stage, ChatStage::ContextAssembled);

        // 4. ProviderRequest — session_id surfaces.
        p = update_progress(
            p,
            &ChatEvent::ProviderRequest {
                ts: now_ts(),
                provider: "codex".into(),
                model: "gpt-5.5".into(),
                prompt_bytes: 1024,
                attachments: 0,
                session_id: Some("01H".into()),
            },
        );
        assert_eq!(p.as_ref().unwrap().session_id.as_deref(), Some("01H"));

        // 5. Phase(ProviderAwait)
        p = update_progress(
            p,
            &ChatEvent::Phase {
                ts: now_ts(),
                stage: ChatStage::ProviderAwait,
                elapsed_s: 0.5,
            },
        );
        assert_eq!(p.as_ref().unwrap().stage, ChatStage::ProviderAwait);

        // 6. ProviderResponse — resumed flag surfaces.
        p = update_progress(
            p,
            &ChatEvent::ProviderResponse {
                ts: now_ts(),
                elapsed_s: 2.0,
                tokens_in: Some(100),
                tokens_out: Some(50),
                resumed: true,
                session_id: Some("01H".into()),
            },
        );
        assert_eq!(p.as_ref().unwrap().resumed, Some(true));

        // 7. Phase(Saved)
        p = update_progress(
            p,
            &ChatEvent::Phase {
                ts: now_ts(),
                stage: ChatStage::Saved,
                elapsed_s: 2.05,
            },
        );
        assert_eq!(p.as_ref().unwrap().stage, ChatStage::Saved);

        // 8. TurnPersisted — terminal; progress clears.
        p = update_progress(p, &ChatEvent::TurnPersisted { ts: now_ts(), codex_turn: 8 });
        assert!(p.is_none(), "TurnPersisted must clear progress");
    }

    #[test]
    fn update_progress_provider_error_clears_progress() {
        let p = update_progress(
            None,
            &ChatEvent::AnalystAppended { ts: now_ts(), turn: 1 },
        );
        let p = update_progress(
            p,
            &ChatEvent::ProviderError {
                ts: now_ts(),
                kind: "io".into(),
                message: "boom".into(),
            },
        );
        assert!(p.is_none());
    }

    #[test]
    fn update_progress_cancel_clears_progress() {
        let p = update_progress(
            None,
            &ChatEvent::AnalystAppended { ts: now_ts(), turn: 1 },
        );
        let p = update_progress(
            p,
            &ChatEvent::Cancelled { ts: now_ts(), by: CancelSource::EscKey },
        );
        assert!(p.is_none());
    }

    #[test]
    fn update_progress_ignores_lifecycle_and_input_events() {
        // SessionOpened, KeyCommand, EvidenceAttached etc. don't change progress.
        let p0 = update_progress(None, &ChatEvent::SessionOpened {
            ticket_id: "1".into(), ts: now_ts(),
        });
        assert!(p0.is_none());
        let p1 = update_progress(None, &ChatEvent::KeyCommand {
            ts: now_ts(), command: "send".into(),
        });
        assert!(p1.is_none());
    }

    #[test]
    fn advance_progress_tick_derives_frame_idx_from_elapsed() {
        let p = ChatProgress {
            stage: ChatStage::ProviderAwait,
            canned_msg: "asking around…",
            elapsed_s: 0.0,
            frame_idx: 0,
            resumed: None,
            session_id: None,
        };
        // Two ticks with identical elapsed_s → identical frame_idx (deterministic).
        let a = advance_progress_tick(p.clone(), 1.2);
        let b = advance_progress_tick(p.clone(), 1.2);
        assert_eq!(a.frame_idx, b.frame_idx);
        // Different elapsed → different frame_idx (eventually).
        let c = advance_progress_tick(p.clone(), 0.0);
        let d = advance_progress_tick(p.clone(), 0.5);
        assert!(c.frame_idx != d.frame_idx || c.elapsed_s != d.elapsed_s);
        assert_eq!(d.elapsed_s, 0.5);
    }

    #[test]
    fn advance_progress_tick_rotates_canned_message_for_provider_await() {
        let mut p = ChatProgress {
            stage: ChatStage::ProviderAwait,
            canned_msg: "",
            elapsed_s: 0.0,
            frame_idx: 0,
            resumed: None,
            session_id: None,
        };
        let messages: Vec<&str> = (0..16)
            .map(|i| {
                p = advance_progress_tick(p.clone(), i as f64 * 4.0);
                p.canned_msg
            })
            .collect();
        let unique: std::collections::HashSet<_> = messages.iter().copied().collect();
        assert_eq!(unique.len(), 4, "expected 4 distinct rotations: {messages:?}");
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cd triage-cli-rs && cargo test --lib chat::tests::update_progress chat::tests::advance_progress 2>&1 | head -20`
Expected: FAIL — `cannot find struct 'ChatProgress'`, `cannot find function 'update_progress'`, `cannot find function 'advance_progress_tick'`.

- [ ] **Step 3: Implement ChatProgress + state-machine functions**

Append to `triage-cli-rs/src/chat.rs` after the `ChatEvent` enum:

```rust
/// The number of braille throbber frames cycled through while a call is
/// in flight. Shared between `tui/chat.rs` (which renders the frame) and
/// `advance_progress_tick` (which derives the index from `elapsed_s`).
pub const THROBBER_FRAME_COUNT: usize = 10;

/// Snapshot of in-flight chat state that drives the progress banner. One
/// instance lives at a time, in `tui::inbox::run_chat_session`. Mutated
/// by `update_progress` on incoming events and by `advance_progress_tick`
/// on each event-loop tick.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatProgress {
    pub stage: ChatStage,
    /// Stage-mapped canned text. Rotates only while in `ProviderAwait`;
    /// fixed for every other stage.
    pub canned_msg: &'static str,
    /// Wall-clock seconds since the turn started (not since the current
    /// phase started). Drives both the elapsed label and the rotation
    /// cosmetic.
    pub elapsed_s: f64,
    /// Index into `tui::chat::THROBBER_FRAMES`. Computed from `elapsed_s`
    /// on every tick so the spinner stays smooth even if events are slow.
    pub frame_idx: usize,
    /// Populated by `ProviderResponse`. None until then.
    pub resumed: Option<bool>,
    /// Populated by `ProviderRequest` and `ProviderResponse`. None until then.
    pub session_id: Option<String>,
}

/// Folds an incoming `ChatEvent` into the running `ChatProgress`. Returns
/// `None` when the turn has ended (or hasn't begun). Lifecycle and input
/// events that are not part of a turn (`SessionOpened`, `KeyCommand`,
/// `EvidenceAttached`, …) leave `prev` unchanged.
///
/// Pure function — testable without spawning a task. Called by the inbox
/// event loop on each drained event.
pub fn update_progress(prev: Option<ChatProgress>, evt: &ChatEvent) -> Option<ChatProgress> {
    match evt {
        ChatEvent::AnalystAppended { .. } => {
            // Open progress as a placeholder so the banner shows up immediately
            // after the analyst hits send — even before the first Phase fires.
            // Use Ingesting as the placeholder stage so the canned text reads
            // "loading attachments…" while the spawned task does that work.
            Some(prev.unwrap_or(ChatProgress {
                stage: ChatStage::Ingesting,
                canned_msg: canned_message(ChatStage::Ingesting, 0),
                elapsed_s: 0.0,
                frame_idx: 0,
                resumed: None,
                session_id: None,
            }))
        }
        ChatEvent::Phase { stage, elapsed_s, .. } => {
            let base = prev.unwrap_or(ChatProgress {
                stage: *stage,
                canned_msg: canned_message(*stage, 0),
                elapsed_s: *elapsed_s,
                frame_idx: 0,
                resumed: None,
                session_id: None,
            });
            Some(ChatProgress {
                stage: *stage,
                canned_msg: canned_message(*stage, (*elapsed_s / 4.0) as usize),
                elapsed_s: *elapsed_s,
                ..base
            })
        }
        ChatEvent::ProviderRequest { session_id, .. } => {
            prev.map(|p| ChatProgress {
                session_id: session_id.clone().or(p.session_id),
                ..p
            })
        }
        ChatEvent::ProviderResponse {
            resumed,
            session_id,
            elapsed_s,
            ..
        } => prev.map(|p| ChatProgress {
            resumed: Some(*resumed),
            session_id: session_id.clone().or(p.session_id),
            elapsed_s: *elapsed_s,
            ..p
        }),
        // Terminal events — clear progress.
        ChatEvent::TurnPersisted { .. }
        | ChatEvent::ProviderError { .. }
        | ChatEvent::Cancelled { .. } => None,
        // Lifecycle and input events do not affect in-flight progress.
        ChatEvent::SessionOpened { .. }
        | ChatEvent::SessionClosed { .. }
        | ChatEvent::KeyCommand { .. }
        | ChatEvent::EvidenceAttached { .. }
        | ChatEvent::EvidenceRejected { .. } => prev,
    }
}

/// Advance the per-frame view fields (`frame_idx`, `canned_msg`, `elapsed_s`)
/// based on wall-clock elapsed seconds. Called once per draw tick. Pure —
/// the new struct is independent of the previous `frame_idx`, so we don't
/// depend on tick cadence (a slow tick won't desync the spinner).
pub fn advance_progress_tick(prev: ChatProgress, elapsed_s: f64) -> ChatProgress {
    let frame_idx = ((elapsed_s * 12.5) as usize) % THROBBER_FRAME_COUNT;
    let rotation_idx = (elapsed_s / 4.0) as usize;
    ChatProgress {
        elapsed_s,
        frame_idx,
        canned_msg: canned_message(prev.stage, rotation_idx),
        ..prev
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd triage-cli-rs && cargo test --lib "chat::tests::update_progress" "chat::tests::advance_progress" 2>&1 | tail -20`
Expected: PASS — six tests pass.

- [ ] **Step 5: Commit**

```bash
git add triage-cli-rs/src/chat.rs
git commit -m "feat(chat): add ChatProgress + update_progress state machine

Pure-function state machine that folds ChatEvents into a running
ChatProgress for the banner widget. advance_progress_tick derives
the spinner frame from wall-clock elapsed so the spinner is smooth
regardless of tick cadence.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 4: ChatLogger — per-ticket JSONL writer

**Files:**
- Modify: `triage-cli-rs/src/chat.rs`
- Test: `triage-cli-rs/src/chat.rs` (inline)

- [ ] **Step 1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block:

```rust
    // ── ChatLogger ──────────────────────────────────────────────────

    #[test]
    fn chat_logger_round_trips_six_events() {
        let dir = tempdir().unwrap();
        // Ensure the .session dir exists (ChatLogger::open should create it).
        let ticket_dir = dir.path().to_path_buf();
        let mut logger = ChatLogger::open(&ticket_dir).unwrap();

        let events = vec![
            ChatEvent::SessionOpened { ticket_id: "44776".into(), ts: now_ts() },
            ChatEvent::KeyCommand { ts: now_ts(), command: "send".into() },
            ChatEvent::AnalystAppended { ts: now_ts(), turn: 1 },
            ChatEvent::Phase { ts: now_ts(), stage: ChatStage::ProviderAwait, elapsed_s: 0.5 },
            ChatEvent::TurnPersisted { ts: now_ts(), codex_turn: 2 },
            ChatEvent::SessionClosed { ts: now_ts(), reason: SessionCloseReason::UserQuit },
        ];
        for evt in &events {
            logger.log(evt);
        }
        drop(logger); // ensure flush + close

        let path = chat_events_log_path(&ticket_dir);
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), events.len());
        for (line, expected) in lines.iter().zip(events.iter()) {
            let back: ChatEvent = serde_json::from_str(line).unwrap();
            assert_eq!(&back, expected);
        }
    }

    #[test]
    fn chat_logger_appends_across_opens() {
        let dir = tempdir().unwrap();
        let ticket_dir = dir.path().to_path_buf();

        {
            let mut logger = ChatLogger::open(&ticket_dir).unwrap();
            logger.log(&ChatEvent::KeyCommand { ts: now_ts(), command: "send".into() });
        }
        {
            let mut logger = ChatLogger::open(&ticket_dir).unwrap();
            logger.log(&ChatEvent::KeyCommand { ts: now_ts(), command: "/quit".into() });
        }

        let body = std::fs::read_to_string(chat_events_log_path(&ticket_dir)).unwrap();
        assert_eq!(body.lines().count(), 2, "second open should append, not truncate");
    }

    #[test]
    fn chat_events_log_path_is_inside_session_dir() {
        let dir = tempdir().unwrap();
        let p = chat_events_log_path(dir.path());
        assert_eq!(p, session_dir(dir.path()).join("chat-events.log"));
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cd triage-cli-rs && cargo test --lib chat::tests::chat_logger 2>&1 | head -30`
Expected: FAIL — `cannot find type 'ChatLogger'`, `cannot find function 'chat_events_log_path'`.

- [ ] **Step 3: Implement ChatLogger + chat_events_log_path**

Append to `triage-cli-rs/src/chat.rs`:

```rust
/// Path to the per-ticket chat-events JSONL log at
/// `<ticket_dir>/.session/chat-events.log`.
pub fn chat_events_log_path(ticket_dir: &Path) -> PathBuf {
    session_dir(ticket_dir).join("chat-events.log")
}

/// Append-only JSONL writer for `ChatEvent`. One file per ticket at
/// [`chat_events_log_path`]. Per-event `flush()` keeps the tail usable
/// if the process is killed mid-turn (same rationale as `append_turn`'s
/// `sync_all`). Logging failures are deliberately silent: a chat turn
/// must not fail because the log is unwriteable.
pub struct ChatLogger {
    writer: Option<std::io::BufWriter<fs::File>>,
}

impl ChatLogger {
    /// Open (or create) the chat-events log under `ticket_dir`. Creates the
    /// `.session/` directory if needed. Returns a logger whose `log()` is
    /// a no-op if the file cannot be opened.
    pub fn open(ticket_dir: &Path) -> Result<Self, ChatError> {
        let sdir = session_dir(ticket_dir);
        fs::create_dir_all(&sdir)?;
        let path = chat_events_log_path(ticket_dir);
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            writer: Some(std::io::BufWriter::new(file)),
        })
    }

    /// Append one event to the log. Best-effort: a write or serde failure
    /// is silently swallowed. The caller never sees the error — chat must
    /// keep working even when the log file is full / unwriteable.
    pub fn log(&mut self, evt: &ChatEvent) {
        let Some(w) = self.writer.as_mut() else { return };
        if let Ok(line) = serde_json::to_string(evt) {
            let _ = writeln!(w, "{line}");
            let _ = w.flush();
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd triage-cli-rs && cargo test --lib chat::tests::chat_logger chat::tests::chat_events_log 2>&1 | tail -20`
Expected: PASS — three tests pass.

- [ ] **Step 5: Commit**

```bash
git add triage-cli-rs/src/chat.rs
git commit -m "feat(chat): add ChatLogger writing JSONL chat events

Per-ticket .session/chat-events.log writer. Per-event flush so a
killed process leaves a usable tail. Logging is best-effort: a
disk failure must not abort a chat turn.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 5: ChatPhaseReporter trait + MpscPhaseReporter

**Files:**
- Modify: `triage-cli-rs/src/chat.rs`
- Test: `triage-cli-rs/src/chat.rs` (inline)

- [ ] **Step 1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block:

```rust
    // ── ChatPhaseReporter ───────────────────────────────────────────

    #[tokio::test]
    async fn mpsc_phase_reporter_emits_phase_events_in_order() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
        let reporter = MpscPhaseReporter::new(tx);
        reporter.phase(ChatStage::ContextAssembled);
        reporter.phase(ChatStage::ProviderAwait);
        reporter.phase(ChatStage::ResponseParsed);

        // Drop the reporter to close the channel.
        drop(reporter);

        let mut got: Vec<ChatStage> = Vec::new();
        while let Some(evt) = rx.recv().await {
            match evt {
                ChatEvent::Phase { stage, .. } => got.push(stage),
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(
            got,
            vec![
                ChatStage::ContextAssembled,
                ChatStage::ProviderAwait,
                ChatStage::ResponseParsed,
            ]
        );
    }

    #[tokio::test]
    async fn mpsc_phase_reporter_does_not_panic_on_closed_channel() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
        drop(rx); // close the receiver first
        let reporter = MpscPhaseReporter::new(tx);
        reporter.phase(ChatStage::Ingesting); // must not panic
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cd triage-cli-rs && cargo test --lib chat::tests::mpsc_phase_reporter 2>&1 | head -20`
Expected: FAIL — `cannot find type 'MpscPhaseReporter'`, `cannot find trait 'ChatPhaseReporter'`.

- [ ] **Step 3: Implement ChatPhaseReporter trait + MpscPhaseReporter**

Append to `triage-cli-rs/src/chat.rs`:

```rust
/// Pipeline-facing surface for reporting `ChatStage` transitions. Mirrors
/// the existing `pipeline::Reporter` pattern for the structured pipeline.
/// `pipeline::followup_turn` accepts an `Option<&dyn ChatPhaseReporter>`
/// and calls `phase()` at five real boundaries.
pub trait ChatPhaseReporter: Send + Sync {
    fn phase(&self, stage: ChatStage);
}

/// `ChatPhaseReporter` implementation that pushes `ChatEvent::Phase`
/// records into an mpsc channel. One reporter is built per turn by
/// `tui::inbox::send_analyst_turn_with_progress`; the `started` clock
/// is captured at construction so `elapsed_s` is relative to turn start.
///
/// Send failures (closed channel) are silently dropped — by the time the
/// receiver is gone the chat session has ended and the events are no
/// longer interesting.
pub struct MpscPhaseReporter {
    tx: tokio::sync::mpsc::UnboundedSender<ChatEvent>,
    started: std::time::Instant,
}

impl MpscPhaseReporter {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<ChatEvent>) -> Self {
        Self {
            tx,
            started: std::time::Instant::now(),
        }
    }
}

impl ChatPhaseReporter for MpscPhaseReporter {
    fn phase(&self, stage: ChatStage) {
        let _ = self.tx.send(ChatEvent::Phase {
            ts: chrono::Utc::now(),
            stage,
            elapsed_s: self.started.elapsed().as_secs_f64(),
        });
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd triage-cli-rs && cargo test --lib chat::tests::mpsc_phase_reporter 2>&1 | tail -20`
Expected: PASS — two tokio tests pass.

- [ ] **Step 5: Commit**

```bash
git add triage-cli-rs/src/chat.rs
git commit -m "feat(chat): add ChatPhaseReporter trait + MpscPhaseReporter impl

Bridges pipeline::followup_turn (synchronous phase boundaries) and
the tokio mpsc channel the inbox event loop drains. Send failures
on a closed channel are silently dropped.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 6: pipeline::followup_turn — accept Option<&dyn ChatPhaseReporter>

**Files:**
- Modify: `triage-cli-rs/src/pipeline.rs:954-1170` (the `followup_turn` function body)
- Modify: `triage-cli-rs/src/tui/inbox.rs:2237-2247` (the call site in `send_analyst_turn`)
- Modify: `triage-cli-rs/src/pipeline.rs` test module (existing followup_turn tests that pass arguments)
- Test: `triage-cli-rs/src/pipeline.rs` (inline)

- [ ] **Step 1: Write failing test for the phase emission contract**

Add to the `#[cfg(test)] mod tests` block at the bottom of `pipeline.rs` (use the existing test setup helpers — `make_test_provider`, mock provider — by reading the existing followup_turn tests around line 1496 first to match conventions):

```rust
    use crate::chat::{ChatPhaseReporter, ChatStage};

    #[derive(Default)]
    struct RecordingReporter {
        stages: std::sync::Mutex<Vec<ChatStage>>,
    }

    impl ChatPhaseReporter for RecordingReporter {
        fn phase(&self, stage: ChatStage) {
            self.stages.lock().unwrap().push(stage);
        }
    }

    #[tokio::test]
    async fn followup_turn_emits_five_phases_in_order_with_codex_resume() {
        // Build a minimal ticket folder with one prior codex turn that
        // carries a session_id so SessionResumeAttempt fires.
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().to_path_buf();
        std::fs::create_dir_all(crate::chat::session_dir(&ticket_dir)).unwrap();
        // Append a prior codex turn with a session_id so resume is attempted.
        let prior = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: "1".into(),
            turn: 1,
            turn_kind: crate::models::TurnKind::Codex,
            ts: chrono::Utc::now(),
            author: None,
            body: "first answer".into(),
            evidence: vec![],
            provider: Some("codex".into()),
            model: Some("gpt-5.5".into()),
            tokens_in: None,
            tokens_out: None,
            elapsed_s: Some(0.1),
            session_id: Some("01HFAKEPRIOR".into()),
            resumed: Some(false),
            action: None,
            outcome: None,
            drove_revision_from_turns: None,
            diff: None,
        };
        crate::chat::append_turn(
            &crate::chat::conversation_jsonl_path(&ticket_dir),
            &prior,
        )
        .unwrap();

        // Stub provider whose followup() returns immediately with resumed=true.
        struct StubProvider;
        impl crate::providers::LlmProvider for StubProvider {
            fn name(&self) -> &'static str { "stub" }
            fn complete<'a>(
                &'a self, _: &'a str, _: &'a str, _: &'a str,
            ) -> std::pin::Pin<Box<dyn std::future::Future<
                Output = Result<crate::providers::CompletionResult, crate::providers::ProviderError>
            > + Send + 'a>> {
                Box::pin(async { unreachable!() })
            }
            fn followup<'a>(
                &'a self, _sid: Option<&'a str>, _p: &'a str, _sys: &'a str, _m: &'a str,
                _att: &'a [crate::models::Attachment],
            ) -> std::pin::Pin<Box<dyn std::future::Future<
                Output = Result<crate::providers::FollowupResult, crate::providers::ProviderError>
            > + Send + 'a>> {
                Box::pin(async move {
                    Ok(crate::providers::FollowupResult {
                        text: "ok".into(),
                        tokens_in: None,
                        tokens_out: None,
                        session_id: Some("01HFAKENEW".into()),
                        resumed: true,
                    })
                })
            }
        }

        let reporter = RecordingReporter::default();
        let _ = followup_turn(
            &ticket_dir, "1", "what next?", "",
            "gpt-5.5", &[], &StubProvider, Some(&reporter),
        )
        .await
        .expect("followup_turn must succeed");

        let stages = reporter.stages.lock().unwrap().clone();
        assert_eq!(
            stages,
            vec![
                ChatStage::ContextAssembled,
                ChatStage::SessionResumeAttempt,
                ChatStage::ProviderAwait,
                ChatStage::ResponseParsed,
                ChatStage::Saved,
            ]
        );
    }

    #[tokio::test]
    async fn followup_turn_skips_session_resume_when_no_prior_session() {
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().to_path_buf();
        std::fs::create_dir_all(crate::chat::session_dir(&ticket_dir)).unwrap();
        // No prior turn — no session id — SessionResumeAttempt must be skipped.

        struct StubProvider;
        impl crate::providers::LlmProvider for StubProvider {
            fn name(&self) -> &'static str { "stub" }
            fn complete<'a>(
                &'a self, _: &'a str, _: &'a str, _: &'a str,
            ) -> std::pin::Pin<Box<dyn std::future::Future<
                Output = Result<crate::providers::CompletionResult, crate::providers::ProviderError>
            > + Send + 'a>> { Box::pin(async { unreachable!() }) }
            fn followup<'a>(
                &'a self, _sid: Option<&'a str>, _p: &'a str, _sys: &'a str, _m: &'a str,
                _att: &'a [crate::models::Attachment],
            ) -> std::pin::Pin<Box<dyn std::future::Future<
                Output = Result<crate::providers::FollowupResult, crate::providers::ProviderError>
            > + Send + 'a>> {
                Box::pin(async move {
                    Ok(crate::providers::FollowupResult {
                        text: "ok".into(),
                        tokens_in: None,
                        tokens_out: None,
                        session_id: None,
                        resumed: false,
                    })
                })
            }
        }

        let reporter = RecordingReporter::default();
        let _ = followup_turn(
            &ticket_dir, "1", "first question", "",
            "gpt-5.5", &[], &StubProvider, Some(&reporter),
        )
        .await
        .expect("followup_turn must succeed");

        let stages = reporter.stages.lock().unwrap().clone();
        assert_eq!(
            stages,
            vec![
                ChatStage::ContextAssembled,
                ChatStage::ProviderAwait,
                ChatStage::ResponseParsed,
                ChatStage::Saved,
            ],
            "SessionResumeAttempt should be skipped when there is no prior session"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cd triage-cli-rs && cargo test --lib pipeline::tests::followup_turn_emits 2>&1 | head -30`
Expected: FAIL — `expected 7 arguments, found 8` (followup_turn doesn't accept reporter yet).

- [ ] **Step 3: Extend `followup_turn` signature and add the 5 phase emission points**

In `triage-cli-rs/src/pipeline.rs`, change the function signature at line 954 from:

```rust
pub async fn followup_turn(
    ticket_dir: &std::path::Path,
    ticket_id: &str,
    prompt: &str,
    system_prompt: &str,
    model: &str,
    attachments: &[crate::models::Attachment],
    provider: &dyn crate::providers::LlmProvider,
) -> Result<crate::providers::FollowupResult, PipelineError> {
```

to:

```rust
pub async fn followup_turn(
    ticket_dir: &std::path::Path,
    ticket_id: &str,
    prompt: &str,
    system_prompt: &str,
    model: &str,
    attachments: &[crate::models::Attachment],
    provider: &dyn crate::providers::LlmProvider,
    reporter: Option<&dyn crate::chat::ChatPhaseReporter>,
) -> Result<crate::providers::FollowupResult, PipelineError> {
```

Then emit phases at the five sites. After the `combined_system_prompt` block ends (currently around line 1042, just before `// Call provider`), add:

```rust
    if let Some(r) = reporter { r.phase(crate::chat::ChatStage::ContextAssembled); }
```

Just before the `provider.followup(` call (currently around line 1046), wrap the `await` with two phase emissions:

```rust
    if let Some(r) = reporter {
        if last_codex_session.is_some() {
            r.phase(crate::chat::ChatStage::SessionResumeAttempt);
        }
        r.phase(crate::chat::ChatStage::ProviderAwait);
    }
    let started = std::time::Instant::now();
    let result = provider
        .followup(
            last_codex_session.as_deref(),
            &redacted_prompt,
            &combined_system_prompt,
            model,
            attachments,
        )
        .await
        .map_err(FollowupError::Provider)?;
    let elapsed_s = started.elapsed().as_secs_f64();

    if let Some(r) = reporter { r.phase(crate::chat::ChatStage::ResponseParsed); }
```

After the `write_conversation_md(...)` call (currently around line 1124, just before the manifest-update block), add:

```rust
    if let Some(r) = reporter { r.phase(crate::chat::ChatStage::Saved); }
```

- [ ] **Step 4: Update existing callers and tests to pass `None`**

In `triage-cli-rs/src/tui/inbox.rs`, change the call site at line 2237-2247 from:

```rust
    let _result = pipeline::followup_turn(
        ticket_dir,
        ticket_id,
        &augmented_prompt,
        "",
        &std::env::var("CODEX_MODEL")
            .unwrap_or_else(|_| crate::providers::codex::DEFAULT_CODEX_MODEL.to_string()),
        &attachments,
        provider,
    )
    .await?;
```

to:

```rust
    let _result = pipeline::followup_turn(
        ticket_dir,
        ticket_id,
        &augmented_prompt,
        "",
        &std::env::var("CODEX_MODEL")
            .unwrap_or_else(|_| crate::providers::codex::DEFAULT_CODEX_MODEL.to_string()),
        &attachments,
        provider,
        None,
    )
    .await?;
```

Search for any other call sites and update them the same way:

Run: `grep -n "followup_turn(" /Users/envelazquez/Documents/triage-cli-latest/triage-cli/triage-cli-rs/src/pipeline.rs /Users/envelazquez/Documents/triage-cli-latest/triage-cli/triage-cli-rs/src/tui/inbox.rs 2>&1`

For every existing test in `pipeline.rs` that calls `followup_turn(...)` (lines around 1496, 1591, 1767, 1904, 2084, 2200, 2328, 2447 per the earlier grep), append `, None` to the argument list. Each call to `followup_turn(` in test bodies needs the same `, None` appended.

- [ ] **Step 5: Run all pipeline tests to verify**

Run: `cd triage-cli-rs && cargo test --lib pipeline:: 2>&1 | tail -30`
Expected: PASS — all existing followup_turn tests pass with the new `None` argument; the two new RecordingReporter tests pass with the expected stage sequences.

- [ ] **Step 6: Commit**

```bash
git add triage-cli-rs/src/pipeline.rs triage-cli-rs/src/tui/inbox.rs
git commit -m "feat(pipeline): followup_turn emits ChatStage phases via optional reporter

Adds an optional &dyn ChatPhaseReporter parameter to followup_turn
and emits five phase events at the real pipeline boundaries:
ContextAssembled, SessionResumeAttempt (codex-only), ProviderAwait,
ResponseParsed, Saved. Existing callers pass None and see no
behavior change.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 7: simple_glob_match helper

**Files:**
- Modify: `triage-cli-rs/src/chat.rs`
- Test: `triage-cli-rs/src/chat.rs` (inline)

- [ ] **Step 1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block:

```rust
    // ── simple_glob_match ───────────────────────────────────────────

    #[test]
    fn simple_glob_match_exact_no_wildcard() {
        assert!(simple_glob_match("station.log", "station.log"));
        assert!(!simple_glob_match("station.log", "other.log"));
    }

    #[test]
    fn simple_glob_match_star_extension() {
        assert!(simple_glob_match("*.log", "station.log"));
        assert!(simple_glob_match("*.log", "a.log"));
        assert!(!simple_glob_match("*.log", "station.txt"));
    }

    #[test]
    fn simple_glob_match_leading_and_trailing_star() {
        assert!(simple_glob_match("station*", "station.log"));
        assert!(simple_glob_match("*log*", "preflight.log.gz"));
        assert!(!simple_glob_match("station*", "preflight.log"));
    }

    #[test]
    fn simple_glob_match_question_mark_one_char() {
        assert!(simple_glob_match("a?.log", "a1.log"));
        assert!(simple_glob_match("a?.log", "ab.log"));
        assert!(!simple_glob_match("a?.log", "abc.log"));
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cd triage-cli-rs && cargo test --lib chat::tests::simple_glob_match 2>&1 | head -10`
Expected: FAIL — `cannot find function 'simple_glob_match'`.

- [ ] **Step 3: Implement simple_glob_match**

Append to `triage-cli-rs/src/chat.rs`:

```rust
/// Tiny inline glob matcher used by [`collect_dir_attachments`]. Supports
/// `*` (zero or more characters) and `?` (exactly one character). No `[]`
/// classes, no `**`, no anchoring — the pattern is matched against the
/// full basename. Bringing in `globset` for one wildcard pattern would be
/// overkill; this is ~30 lines, has predictable behavior, and is tested
/// inline.
pub(crate) fn simple_glob_match(pattern: &str, text: &str) -> bool {
    fn rec(p: &[u8], t: &[u8]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (None, Some(_)) => false,
            (Some(b'*'), _) => {
                // Match zero chars or one+ chars.
                rec(&p[1..], t) || (!t.is_empty() && rec(p, &t[1..]))
            }
            (Some(_), None) => p.iter().all(|&b| b == b'*'),
            (Some(b'?'), Some(_)) => rec(&p[1..], &t[1..]),
            (Some(&a), Some(&b)) if a == b => rec(&p[1..], &t[1..]),
            _ => false,
        }
    }
    rec(pattern.as_bytes(), text.as_bytes())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd triage-cli-rs && cargo test --lib chat::tests::simple_glob_match 2>&1 | tail -10`
Expected: PASS — four tests pass.

- [ ] **Step 5: Commit**

```bash
git add triage-cli-rs/src/chat.rs
git commit -m "feat(chat): add simple_glob_match for /dir filter

Tiny inline wildcard matcher (* and ?). Used by the upcoming
collect_dir_attachments. Adding globset for a single optional
arg is overkill — this is 10 lines plus tests.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 8: collect_dir_attachments — directory walker with caps

**Files:**
- Modify: `triage-cli-rs/src/chat.rs`
- Test: `triage-cli-rs/src/chat.rs` (inline)

- [ ] **Step 1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block:

```rust
    // ── collect_dir_attachments ─────────────────────────────────────

    fn write_text_file(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn collect_dir_respects_file_cap() {
        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("logs");
        // 30 .log files of trivial size.
        for i in 0..30 {
            write_text_file(&src_dir.join(format!("a{i:03}.log")), "data");
        }
        let ticket_dir = dir.path().join("ticket");
        let r = collect_dir_attachments(&ticket_dir, 1, &src_dir, false, None, 25, 4 * 1024 * 1024)
            .unwrap();
        assert_eq!(r.attached.len(), 25);
        assert_eq!(r.skipped.len(), 5);
        for s in &r.skipped {
            assert!(matches!(s, DirSkipped::FileCapExceeded { .. }), "wrong skip kind: {s:?}");
        }
    }

    #[test]
    fn collect_dir_respects_size_cap() {
        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("logs");
        // 10 files * ~1 MiB each = 10 MiB total. Cap at 3 MiB → first 3
        // should fit (the 4th would exceed), remaining 7 skipped.
        let payload = "X".repeat(1024 * 1024);
        for i in 0..10 {
            write_text_file(&src_dir.join(format!("a{i}.log")), &payload);
        }
        let ticket_dir = dir.path().join("ticket");
        let r = collect_dir_attachments(&ticket_dir, 1, &src_dir, false, None, 100, 3 * 1024 * 1024)
            .unwrap();
        assert_eq!(r.attached.len(), 3, "expected 3 files within 3 MiB cap, got {}", r.attached.len());
        assert_eq!(r.skipped.len(), 7);
        assert!(r.skipped.iter().all(|s| matches!(s, DirSkipped::SizeCapExceeded { .. })));
    }

    #[test]
    fn collect_dir_glob_filter() {
        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("logs");
        write_text_file(&src_dir.join("a.log"), "x");
        write_text_file(&src_dir.join("b.log"), "x");
        write_text_file(&src_dir.join("notes.txt"), "x");
        let ticket_dir = dir.path().join("ticket");
        let r = collect_dir_attachments(&ticket_dir, 1, &src_dir, false, Some("*.log"), 100, 4 << 20)
            .unwrap();
        assert_eq!(r.attached.len(), 2);
        assert_eq!(r.skipped.len(), 1);
        assert!(matches!(r.skipped[0], DirSkipped::GlobMismatch { .. }));
    }

    #[test]
    fn collect_dir_recursive_vs_single_level() {
        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("logs");
        write_text_file(&src_dir.join("top.log"), "x");
        write_text_file(&src_dir.join("nested/deep.log"), "x");
        let ticket_dir = dir.path().join("ticket");

        let r_flat = collect_dir_attachments(&ticket_dir, 1, &src_dir, false, None, 100, 4 << 20).unwrap();
        assert_eq!(r_flat.attached.len(), 1);
        // Reset the ticket dir between calls so attach_file doesn't dedupe.
        let ticket_dir2 = dir.path().join("ticket2");
        let r_recur = collect_dir_attachments(&ticket_dir2, 1, &src_dir, true, None, 100, 4 << 20).unwrap();
        assert_eq!(r_recur.attached.len(), 2);
    }

    #[test]
    fn collect_dir_type_allowlist_filters_unknown_extensions() {
        let dir = tempdir().unwrap();
        let src_dir = dir.path().join("logs");
        write_text_file(&src_dir.join("good.log"), "x");
        write_text_file(&src_dir.join("blob.bin"), "x");
        let ticket_dir = dir.path().join("ticket");
        let r = collect_dir_attachments(&ticket_dir, 1, &src_dir, false, None, 100, 4 << 20).unwrap();
        assert_eq!(r.attached.len(), 1, "only good.log should attach");
        assert!(r.skipped.iter().any(|s| matches!(s, DirSkipped::UnsupportedType { .. })));
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cd triage-cli-rs && cargo test --lib chat::tests::collect_dir 2>&1 | head -20`
Expected: FAIL — `cannot find function 'collect_dir_attachments'`, `cannot find type 'DirSkipped'`.

- [ ] **Step 3: Implement DirCollectResult, DirSkipped, collect_dir_attachments**

Append to `triage-cli-rs/src/chat.rs`:

```rust
/// Outcome of a `/dir <path>` attachment batch. `attached` are the
/// provenance records added to pending evidence; `skipped` captures
/// every file the walker visited but did not attach, with a reason.
#[derive(Debug)]
pub struct DirCollectResult {
    pub attached: Vec<EvidenceProvenance>,
    pub skipped: Vec<DirSkipped>,
}

#[derive(Debug, PartialEq)]
pub enum DirSkipped {
    /// `cap_files` was already reached; this file was not visited.
    FileCapExceeded { path: PathBuf },
    /// Attaching this file would push the running aggregate past
    /// `cap_total_bytes`; the file was skipped.
    SizeCapExceeded { path: PathBuf, bytes: u64 },
    /// Detected file type was `FileType::Unknown` AND the basename did
    /// not match the extension allow-list (and no explicit glob matched).
    UnsupportedType { path: PathBuf },
    /// An explicit glob was supplied and the basename did not match it.
    GlobMismatch { path: PathBuf },
}

/// Walk `dir` and return a batch of `EvidenceProvenance` for the analyst
/// to attach to the next turn. Single-level by default; opt into recursion
/// with `recursive: true`. Optional `glob` filter is matched against each
/// basename via [`simple_glob_match`]; absent glob falls through to a
/// curated extension allow-list (`txt, log, md, json, csv, yaml, yml,
/// conf, ini, rs, py, ts, tsx, js, jsx`).
///
/// Order is deterministic: `(parent_path, basename)` sort. Files are
/// attached via [`attach_file`] (so sha256, copy-into-ticket, and
/// provenance work identically to the single-file path). The first
/// file that would push the running total past `cap_total_bytes`
/// triggers `SizeCapExceeded` for itself and every subsequent file;
/// `cap_files` is enforced after the size check.
pub fn collect_dir_attachments(
    ticket_dir: &Path,
    turn_no: u32,
    dir: &Path,
    recursive: bool,
    glob: Option<&str>,
    cap_files: usize,
    cap_total_bytes: u64,
) -> Result<DirCollectResult, ChatError> {
    const EXT_ALLOWLIST: &[&str] = &[
        "txt", "log", "md", "json", "csv", "yaml", "yml", "conf", "ini",
        "rs", "py", "ts", "tsx", "js", "jsx",
    ];

    // Walk and collect candidate paths up-front so we can apply ordering
    // and caps deterministically.
    let mut candidates: Vec<PathBuf> = Vec::new();
    walk_dir(dir, recursive, &mut candidates)?;
    candidates.sort();

    let mut attached: Vec<EvidenceProvenance> = Vec::new();
    let mut skipped: Vec<DirSkipped> = Vec::new();
    let mut running_bytes: u64 = 0;
    let mut size_cap_tripped = false;

    for path in candidates {
        let basename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        // Glob / type filter.
        let matches_filter = if let Some(g) = glob {
            simple_glob_match(g, &basename)
        } else {
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            EXT_ALLOWLIST.iter().any(|a| *a == ext)
        };
        if !matches_filter {
            skipped.push(if glob.is_some() {
                DirSkipped::GlobMismatch { path }
            } else {
                DirSkipped::UnsupportedType { path }
            });
            continue;
        }

        // File-count cap: don't even stat the file.
        if attached.len() >= cap_files {
            skipped.push(DirSkipped::FileCapExceeded { path });
            continue;
        }

        // Size cap (sticky — once tripped, every remaining file is skipped).
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size_cap_tripped || running_bytes.saturating_add(bytes) > cap_total_bytes {
            size_cap_tripped = true;
            skipped.push(DirSkipped::SizeCapExceeded { path, bytes });
            continue;
        }

        match attach_file(ticket_dir, turn_no, &path) {
            Ok(prov) => {
                running_bytes = running_bytes.saturating_add(bytes);
                attached.push(prov);
            }
            Err(_) => {
                // attach_file failed (e.g. unreadable file). Treat as skipped.
                skipped.push(DirSkipped::UnsupportedType { path });
            }
        }
    }

    Ok(DirCollectResult { attached, skipped })
}

fn walk_dir(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> Result<(), ChatError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_file() {
            out.push(path);
        } else if ft.is_dir() && recursive {
            walk_dir(&path, true, out)?;
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd triage-cli-rs && cargo test --lib chat::tests::collect_dir 2>&1 | tail -20`
Expected: PASS — five tests pass.

- [ ] **Step 5: Commit**

```bash
git add triage-cli-rs/src/chat.rs
git commit -m "feat(chat): collect_dir_attachments with file/byte caps and glob filter

Single-level by default with recursive opt-in. Extension allow-list
when no glob supplied. Sticky byte cap (once tripped, every
remaining file is skipped) so the analyst gets a stable order.
Each accepted file goes through attach_file so provenance + sha256
work identically to the single-file path.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 9: Extend ChatCommand with Dir variant + parser

**Files:**
- Modify: `triage-cli-rs/src/tui/chat.rs:258-299` (the `ChatCommand` enum and `parse_chat_command` function)
- Test: `triage-cli-rs/src/tui/chat.rs` (inline `mod tests`, after the existing `parse_*` tests)

- [ ] **Step 1: Write failing tests**

Add to the `#[cfg(test)] mod tests` block at the bottom of `tui/chat.rs`:

```rust
    #[test]
    fn parse_dir_command_basic() {
        assert_eq!(
            parse_chat_command("/dir ./logs"),
            ChatCommand::Dir {
                path: std::path::PathBuf::from("./logs"),
                recursive: false,
                glob: None,
            }
        );
    }

    #[test]
    fn parse_dir_command_recursive_flag() {
        assert_eq!(
            parse_chat_command("/dir ./logs -r"),
            ChatCommand::Dir {
                path: std::path::PathBuf::from("./logs"),
                recursive: true,
                glob: None,
            }
        );
    }

    #[test]
    fn parse_dir_command_with_glob() {
        assert_eq!(
            parse_chat_command("/dir ./logs *.log"),
            ChatCommand::Dir {
                path: std::path::PathBuf::from("./logs"),
                recursive: false,
                glob: Some("*.log".into()),
            }
        );
    }

    #[test]
    fn parse_dir_command_recursive_and_glob() {
        assert_eq!(
            parse_chat_command("/dir ./logs -r *.log"),
            ChatCommand::Dir {
                path: std::path::PathBuf::from("./logs"),
                recursive: true,
                glob: Some("*.log".into()),
            }
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cd triage-cli-rs && cargo test --lib tui::chat::tests::parse_dir 2>&1 | head -20`
Expected: FAIL — `no variant 'Dir' on type 'ChatCommand'`.

- [ ] **Step 3: Extend ChatCommand enum**

In `triage-cli-rs/src/tui/chat.rs`, replace the existing `ChatCommand` enum (around line 258) with:

```rust
/// Slash commands recognized by the chat input modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatCommand {
    /// `/file <path>` — attach a file by path.
    File(std::path::PathBuf),
    /// `/dir <path> [-r] [glob]` — attach a directory; opt-in recursive
    /// (`-r`) and optional wildcard filter (`*.log`).
    Dir {
        path: std::path::PathBuf,
        recursive: bool,
        glob: Option<String>,
    },
    /// `/paste <label>=<body>` — attach a labeled paste.
    Paste { label: String, body: String },
    /// `/revise` — re-run the structured pipeline.
    Revise,
    /// `/retry` — re-attempt the last failed provider call.
    Retry,
    /// `/quit` — close the chat pane.
    Quit,
    /// A plain analyst body (no leading slash).
    Body(String),
}
```

- [ ] **Step 4: Extend parse_chat_command**

In the same file, add to `parse_chat_command` (around line 276) before the existing `/paste` branch:

```rust
    if let Some(rest) = trimmed.strip_prefix("/dir ") {
        let mut tokens = rest.split_whitespace();
        let path = tokens.next().unwrap_or("").to_string();
        let mut recursive = false;
        let mut glob: Option<String> = None;
        for tok in tokens {
            if tok == "-r" {
                recursive = true;
            } else if glob.is_none() {
                glob = Some(tok.to_string());
            }
        }
        if !path.is_empty() {
            return ChatCommand::Dir {
                path: std::path::PathBuf::from(path),
                recursive,
                glob,
            };
        }
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd triage-cli-rs && cargo test --lib tui::chat::tests::parse 2>&1 | tail -20`
Expected: PASS — four new `parse_dir_*` tests pass, plus the existing `parse_file`, `parse_paste`, `parse_revise_retry_quit`, `parse_plain_body` tests still pass.

- [ ] **Step 6: Commit**

```bash
git add triage-cli-rs/src/tui/chat.rs
git commit -m "feat(tui/chat): parse /dir <path> [-r] [glob] slash command

Adds the Dir variant to ChatCommand with the parser branch.
Implementation of the directory walk lives in chat.rs; this is
just input plumbing.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 10: Replace InFlightState with ChatProgress in ChatPane widget

**Files:**
- Modify: `triage-cli-rs/src/tui/chat.rs` (the `ChatPane` struct, `InFlightState`, `render_status_line`, plus the existing snapshot test that constructs `ChatPane`)
- Modify: `triage-cli-rs/src/tui/inbox.rs` (all references to `crate::tui::chat::InFlightState` — replace with `crate::chat::ChatProgress`)

- [ ] **Step 1: Write failing tests for the new ChatProgress-based rendering**

Add to the `#[cfg(test)] mod tests` block in `tui/chat.rs`:

```rust
    fn chat_progress(stage: crate::chat::ChatStage, elapsed_s: f64) -> crate::chat::ChatProgress {
        crate::chat::ChatProgress {
            stage,
            canned_msg: crate::chat::canned_message(stage, 0),
            elapsed_s,
            frame_idx: 0,
            resumed: None,
            session_id: None,
        }
    }

    #[test]
    fn pane_renders_canned_message_for_each_stage() {
        let stages = [
            crate::chat::ChatStage::Ingesting,
            crate::chat::ChatStage::ContextAssembled,
            crate::chat::ChatStage::SessionResumeAttempt,
            crate::chat::ChatStage::ProviderAwait,
            crate::chat::ChatStage::ResponseParsed,
            crate::chat::ChatStage::Saved,
        ];
        for stage in stages {
            let progress = chat_progress(stage, 1.0);
            let input = TextArea::default();
            let pane = ChatPane {
                turns: &[],
                input: ChatInputSurface::Ask(&input),
                ticket_id: "1",
                progress: Some(&progress),
                status_hint: None,
                transcript_scroll: 0,
                transcript_follow_bottom: true,
            };
            let backend = TestBackend::new(80, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| f.render_widget(&pane, f.area())).unwrap();
            let dump = buffer_to_strings(terminal.backend().buffer());
            let joined = dump.join("\n");
            let expected_msg = crate::chat::canned_message(stage, 0);
            assert!(
                joined.contains(expected_msg),
                "expected canned message {expected_msg:?} for {stage:?}; dump:\n{joined}"
            );
        }
    }

    #[test]
    fn pane_renders_at_four_heights_without_panic() {
        let progress = chat_progress(crate::chat::ChatStage::ProviderAwait, 2.3);
        for height in [8u16, 12, 18, 28] {
            let input = TextArea::default();
            let pane = ChatPane {
                turns: &[],
                input: ChatInputSurface::Ask(&input),
                ticket_id: "1",
                progress: Some(&progress),
                status_hint: None,
                transcript_scroll: 0,
                transcript_follow_bottom: true,
            };
            let backend = TestBackend::new(80, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| f.render_widget(&pane, f.area())).unwrap();
            // No panic, no clipping crash. Spot-check: canned text is present at
            // all four tiers (single-row fallback still renders it).
            let dump = buffer_to_strings(terminal.backend().buffer());
            assert!(
                dump.iter().any(|l| l.contains("asking around")),
                "height {height}: canned text missing in dump:\n{}",
                dump.join("\n")
            );
        }
    }

    #[test]
    fn pane_without_progress_omits_banner() {
        let input = TextArea::default();
        let pane = ChatPane {
            turns: &[],
            input: ChatInputSurface::Ask(&input),
            ticket_id: "1",
            progress: None,
            status_hint: None,
            transcript_scroll: 0,
            transcript_follow_bottom: true,
        };
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| f.render_widget(&pane, f.area())).unwrap();
        let dump = buffer_to_strings(terminal.backend().buffer());
        let joined = dump.join("\n");
        // The banner title row contains "codex follow-up". Without progress
        // that row must NOT be drawn.
        assert!(
            !joined.contains("codex follow-up"),
            "banner leaked into a no-progress draw:\n{joined}"
        );
    }
```

Update the existing `snapshot_chat_pane_renders_three_turns` test (lines 332-365) — change `in_flight: None` to `progress: None`:

```rust
        let pane = ChatPane {
            turns: &turns,
            input: ChatInputSurface::Ask(&input),
            ticket_id: "44776",
            progress: None,
            status_hint: None,
            transcript_scroll: 0,
            transcript_follow_bottom: true,
        };
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cd triage-cli-rs && cargo test --lib tui::chat::tests 2>&1 | head -30`
Expected: FAIL — `no field 'progress' on type 'ChatPane'`, `no field 'in_flight'` removed.

- [ ] **Step 3: Refactor ChatPane to use ChatProgress + add render_phase_banner + banner_rows**

In `triage-cli-rs/src/tui/chat.rs`:

A. Replace the `ChatPane` struct (around line 33-44) with:

```rust
pub struct ChatPane<'a> {
    pub turns: &'a [Turn],
    pub input: ChatInputSurface<'a>,
    pub ticket_id: &'a str,
    /// Drives the progress banner above the input box. `None` when no
    /// call is in flight — banner row(s) are returned to the input.
    pub progress: Option<&'a crate::chat::ChatProgress>,
    /// Shown on the status line when no provider call is in flight (warnings).
    pub status_hint: Option<&'a str>,
    /// Vertical transcript scroll (line offset). Ignored when `follow_bottom` is true.
    pub transcript_scroll: u16,
    /// When true, scroll is computed each frame so the newest turns stay visible.
    pub transcript_follow_bottom: bool,
}
```

B. Delete the existing `InFlightState` struct (around line 46-50).

C. Replace the `impl<'a> Widget for &ChatPane<'a>` block (lines 52-79) with a layout that consults `progress.is_some()` to allocate banner rows:

```rust
impl<'a> Widget for &ChatPane<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let banner_h = if self.progress.is_some() {
            banner_rows(area.height)
        } else {
            0
        };

        // Constraint layout: transcript fills, banner is optional fixed rows,
        // input is 5 rows when there's room (3 when very tight), status is
        // 1 row, command bar is 1 row.
        let input_h: u16 = if area.height >= 14 { 5 } else { 3 };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),                // transcript
                Constraint::Length(banner_h),      // banner (may be 0)
                Constraint::Length(input_h),       // input
                Constraint::Length(1),             // status hint / single-row fallback
                Constraint::Length(1),             // command bar
            ])
            .split(area);

        render_transcript(
            self.turns,
            chunks[0],
            buf,
            self.transcript_scroll,
            self.transcript_follow_bottom,
        );
        if let Some(p) = self.progress {
            render_phase_banner(p, chunks[1], buf);
        }
        render_input(&self.input, chunks[2], buf);
        render_status_line(self.progress, self.status_hint, chunks[3], buf);
        render_command_bar(chunks[4], buf);
    }
}

/// Pick the banner height in rows from the available terminal height.
/// Four tiers: full bordered (≥20), compact bordered (≥14), tight
/// unbordered (≥10), single-row fallback (<10).
fn banner_rows(area_height: u16) -> u16 {
    match area_height {
        h if h >= 20 => 4,
        h if h >= 14 => 3,
        h if h >= 10 => 2,
        _ => 1,
    }
}
```

D. Replace the existing `render_status_line` (line 187-215) with one that takes `Option<&ChatProgress>`:

```rust
fn render_status_line(
    progress: Option<&crate::chat::ChatProgress>,
    status_hint: Option<&str>,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // When progress exists we let the banner own the live feedback. The
    // status line shows the hint (or stays blank).
    let (text, color) = match progress {
        Some(p) if banner_rows(area.height + 1) == 1 => {
            // Tight-tier fallback: status line IS the banner.
            let frame = THROBBER_FRAMES[p.frame_idx % THROBBER_FRAMES.len()];
            (
                format!(
                    " {frame} {} {:.1}s elapsed (Esc to cancel)",
                    p.canned_msg, p.elapsed_s
                ),
                stage_color(p.stage),
            )
        }
        Some(_) => (status_hint.unwrap_or("").to_string(), SYSTEM_HEADER),
        None => (status_hint.unwrap_or("").to_string(), SYSTEM_HEADER),
    };
    Paragraph::new(text)
        .style(Style::default().fg(color))
        .render(area, buf);
}
```

E. Add a new `render_phase_banner` and `stage_color` after `render_status_line`:

```rust
fn render_phase_banner(progress: &crate::chat::ChatProgress, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let frame = THROBBER_FRAMES[progress.frame_idx % THROBBER_FRAMES.len()];
    let color = stage_color(progress.stage);
    let stage_label = stage_label_str(progress.stage);

    match area.height {
        4 => {
            // Full bordered banner with title + 3 content rows.
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" codex follow-up ")
                .style(Style::default().fg(color));
            let inner = block.inner(area);
            block.render(area, buf);
            let line1 = format!(
                "{frame}  {}  stage: {}   elapsed {:.1}s",
                progress.canned_msg, stage_label, progress.elapsed_s
            );
            let line2 = match (progress.resumed, &progress.session_id) {
                (Some(true), Some(sid)) => format!("session: resumed (sid {sid})"),
                (Some(false), Some(sid)) => format!("session: fresh (sid {sid})"),
                (Some(_), None) => "session: in flight".to_string(),
                (None, _) => "session: pending".to_string(),
            };
            let line3 = "Esc cancels · Ctrl-T retries last turn".to_string();
            let lines = vec![Line::from(line1), Line::from(line2), Line::from(line3)];
            Paragraph::new(lines).render(inner, buf);
        }
        3 => {
            // Compact bordered: title + 1 content row + 1 hint row.
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" codex follow-up ")
                .style(Style::default().fg(color));
            let inner = block.inner(area);
            block.render(area, buf);
            let line1 = format!(
                "{frame}  {}  {:.1}s  (Esc cancel)",
                progress.canned_msg, progress.elapsed_s
            );
            Paragraph::new(line1).render(inner, buf);
        }
        2 => {
            // Tight unbordered: separator row + content row.
            let lines = vec![
                Line::from("─".repeat(area.width as usize)),
                Line::from(format!(
                    " {frame}  {}  {:.1}s  (Esc cancel)",
                    progress.canned_msg, progress.elapsed_s
                )),
            ];
            Paragraph::new(lines)
                .style(Style::default().fg(color))
                .render(area, buf);
        }
        _ => {
            // Single-row fallback path: rendered by render_status_line below.
        }
    }
}

fn stage_color(stage: crate::chat::ChatStage) -> Color {
    use crate::chat::ChatStage;
    match stage {
        ChatStage::Ingesting | ChatStage::ContextAssembled => SYSTEM_HEADER,
        ChatStage::SessionResumeAttempt | ChatStage::ProviderAwait => CODEX_HEADER,
        ChatStage::ResponseParsed | ChatStage::Saved => ANALYST_HEADER,
    }
}

fn stage_label_str(stage: crate::chat::ChatStage) -> &'static str {
    use crate::chat::ChatStage;
    match stage {
        ChatStage::Ingesting => "ingesting",
        ChatStage::ContextAssembled => "context_assembled",
        ChatStage::SessionResumeAttempt => "session_resume",
        ChatStage::ProviderAwait => "provider_await",
        ChatStage::ResponseParsed => "response_parsed",
        ChatStage::Saved => "saved",
    }
}
```

F. In `triage-cli-rs/src/tui/inbox.rs`, replace every `crate::tui::chat::InFlightState` with `crate::chat::ChatProgress` and adapt field initializers. Search for the references:

Run: `grep -n "InFlightState" /Users/envelazquez/Documents/triage-cli-latest/triage-cli/triage-cli-rs/src/tui/inbox.rs`

Each occurrence (lines 1729, 1776-1779, 1920-1923, 1962-1965, 1995-1998, 2021-2024, 2051-2054) needs to change. For example, replace this initializer:

```rust
                                in_flight = Some(crate::tui::chat::InFlightState {
                                    elapsed_s: 0.0,
                                    frame_idx: 0,
                                });
```

with:

```rust
                                in_flight = Some(crate::chat::ChatProgress {
                                    stage: crate::chat::ChatStage::Ingesting,
                                    canned_msg: crate::chat::canned_message(
                                        crate::chat::ChatStage::Ingesting, 0,
                                    ),
                                    elapsed_s: 0.0,
                                    frame_idx: 0,
                                    resumed: None,
                                    session_id: None,
                                });
```

And update the variable type at line 1729 from:

```rust
    let mut in_flight: Option<crate::tui::chat::InFlightState> = None;
```

to:

```rust
    let mut in_flight: Option<crate::chat::ChatProgress> = None;
```

And update the `ChatPane` field name from `in_flight:` to `progress:` at line 1794:

```rust
        let pane = crate::tui::chat::ChatPane {
            turns: &outcome.turns,
            input: input_surface,
            ticket_id,
            progress: in_flight.as_ref(),
            status_hint: status_hint.as_deref(),
            transcript_scroll,
            transcript_follow_bottom,
        };
```

And the frame_idx advance at line 1772 — replace the construction with a call to `crate::chat::advance_progress_tick`:

```rust
            } else if let Some(started) = turn_started {
                let elapsed = started.elapsed().as_secs_f64();
                in_flight = in_flight.map(|p| crate::chat::advance_progress_tick(p, elapsed));
            }
```

- [ ] **Step 4: Run all tui tests to verify**

Run: `cd triage-cli-rs && cargo test --lib tui:: 2>&1 | tail -30`
Expected: PASS — existing snapshot test passes with `progress: None`; three new `pane_renders_*` tests pass; `cargo build` succeeds.

Run: `cd triage-cli-rs && cargo build --release 2>&1 | tail -5`
Expected: build succeeds with no errors.

- [ ] **Step 5: Commit**

```bash
git add triage-cli-rs/src/tui/chat.rs triage-cli-rs/src/tui/inbox.rs
git commit -m "feat(tui/chat): responsive progress banner with 4 size tiers

Replaces InFlightState with ChatProgress and adds render_phase_banner
with a banner_rows helper that clamps banner height to terminal
height. Single-source widget for all four tiers (4-row full / 3-row
compact / 2-row tight / 1-row fallback). Resize is invisible — next
draw re-clamps from f.area().

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 11: send_analyst_turn_with_progress — emit ChatEvents from the spawned task

**Files:**
- Modify: `triage-cli-rs/src/tui/inbox.rs:2172-2249` (rewrite `send_analyst_turn`)

This task introduces the ChatEvent-emitting wrapper. The next task (12) wires it into the event loop. This split keeps the changes reviewable.

- [ ] **Step 1: Rename + extend `send_analyst_turn`**

In `triage-cli-rs/src/tui/inbox.rs`, replace `async fn send_analyst_turn(...)` at line 2172 with:

```rust
async fn send_analyst_turn_with_progress(
    ticket_dir: &Path,
    ticket_id: &str,
    body: &str,
    evidence: Vec<crate::models::EvidenceProvenance>,
    provider: &dyn crate::providers::LlmProvider,
    tx: tokio::sync::mpsc::UnboundedSender<crate::chat::ChatEvent>,
) -> anyhow::Result<()> {
    use crate::chat;
    use chrono::Utc;

    let model = std::env::var("CODEX_MODEL")
        .unwrap_or_else(|_| crate::providers::codex::DEFAULT_CODEX_MODEL.to_string());

    let _ = tx.send(crate::chat::ChatEvent::Phase {
        ts: Utc::now(),
        stage: crate::chat::ChatStage::Ingesting,
        elapsed_s: 0.0,
    });

    // Build the augmented prompt and attachments BEFORE moving `evidence`
    // into the turn record below. Pastes are inlined; files become
    // Attachment entries that flow through the provider's native channel.
    let (augmented_prompt, attachments) = build_followup_message(body, &evidence);

    {
        // Acquire the lock BEFORE computing `next` so a concurrent writer
        // can't sneak an append between our read and our own append.
        let _guard = chat::acquire_session_lock(
            &chat::session_dir(ticket_dir),
            std::time::Duration::from_secs(5),
        )
        .map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        let next = next_turn_number(ticket_dir)?;
        let analyst_turn = crate::models::Turn {
            schema: "triage-cli/conversation".into(),
            schema_version: 1,
            ticket_id: ticket_id.to_string(),
            turn: next,
            turn_kind: crate::models::TurnKind::Analyst,
            ts: Utc::now(),
            author: std::env::var("TRIAGE_OWNER")
                .or_else(|_| std::env::var("USER"))
                .or_else(|_| std::env::var("USERNAME"))
                .ok(),
            body: body.to_string(),
            evidence,
            provider: None,
            model: None,
            tokens_in: None,
            tokens_out: None,
            elapsed_s: None,
            session_id: None,
            resumed: None,
            action: None,
            outcome: None,
            drove_revision_from_turns: None,
            diff: None,
        };
        chat::append_turn(&chat::conversation_jsonl_path(ticket_dir), &analyst_turn)
            .map_err(|e| anyhow::anyhow!("append_turn: {e}"))?;
        let parsed = chat::parse_conversation_jsonl(&chat::conversation_jsonl_path(ticket_dir))
            .map_err(|e| anyhow::anyhow!("parse: {e}"))?;
        chat::write_conversation_md(
            &chat::conversation_md_path(ticket_dir),
            &parsed.turns,
            ticket_id,
        )
        .map_err(|e| anyhow::anyhow!("write_md: {e}"))?;

        let _ = tx.send(crate::chat::ChatEvent::AnalystAppended {
            ts: Utc::now(),
            turn: next,
        });
    }

    // Build the reporter once; pipeline emits ContextAssembled,
    // SessionResumeAttempt (when applicable), ProviderAwait, ResponseParsed,
    // Saved through it.
    let reporter = crate::chat::MpscPhaseReporter::new(tx.clone());

    // Emit a ProviderRequest event right before the call (we don't know
    // session_id yet — it's set on the response).
    let _ = tx.send(crate::chat::ChatEvent::ProviderRequest {
        ts: Utc::now(),
        provider: provider.name().to_string(),
        model: model.clone(),
        prompt_bytes: augmented_prompt.len(),
        attachments: attachments.len(),
        session_id: None,
    });

    let started = std::time::Instant::now();
    let result = pipeline::followup_turn(
        ticket_dir,
        ticket_id,
        &augmented_prompt,
        "",
        &model,
        &attachments,
        provider,
        Some(&reporter),
    )
    .await;

    match result {
        Ok(r) => {
            let _ = tx.send(crate::chat::ChatEvent::ProviderResponse {
                ts: Utc::now(),
                elapsed_s: started.elapsed().as_secs_f64(),
                tokens_in: r.tokens_in,
                tokens_out: r.tokens_out,
                resumed: r.resumed,
                session_id: r.session_id.clone(),
            });
            // The pipeline writes the codex turn before returning; emit
            // TurnPersisted as the terminal event so update_progress clears.
            let outcome =
                chat::parse_conversation_jsonl(&chat::conversation_jsonl_path(ticket_dir))?;
            let codex_turn = outcome
                .turns
                .iter()
                .rev()
                .find(|t| matches!(t.turn_kind, crate::models::TurnKind::Codex))
                .map(|t| t.turn)
                .unwrap_or(0);
            let _ = tx.send(crate::chat::ChatEvent::TurnPersisted {
                ts: Utc::now(),
                codex_turn,
            });
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            let (redacted_msg, _) = crate::redact::redact(&msg);
            let _ = tx.send(crate::chat::ChatEvent::ProviderError {
                ts: Utc::now(),
                kind: "followup_turn".into(),
                message: redacted_msg,
            });
            Err(anyhow::anyhow!("{e}"))
        }
    }
}
```

Delete the old `send_analyst_turn` function (the body that ran lines 2172-2249).

- [ ] **Step 2: Update the call sites in `run_chat_session` to use the new name**

In the same file, search for `send_analyst_turn(` (lines 1909, 1984, 2040 per the earlier grep). Each call site currently looks like:

```rust
                                active_job = Some(tokio::spawn(async move {
                                    send_analyst_turn(
                                        &td,
                                        &tid,
                                        &b,
                                        evidence,
                                        provider.as_ref(),
                                    )
                                    .await
                                    .map_err(|e| e.to_string())
                                }));
```

These will break after rename. We add the `tx` parameter in Task 12 once the event loop owns the channel. **For this task only**, change each to pass a dummy channel `let (dummy_tx, _) = tokio::sync::mpsc::unbounded_channel();` to keep the code compiling. Specifically:

```rust
                                let (dummy_tx, _dummy_rx) =
                                    tokio::sync::mpsc::unbounded_channel::<crate::chat::ChatEvent>();
                                active_job = Some(tokio::spawn(async move {
                                    send_analyst_turn_with_progress(
                                        &td,
                                        &tid,
                                        &b,
                                        evidence,
                                        provider.as_ref(),
                                        dummy_tx,
                                    )
                                    .await
                                    .map_err(|e| e.to_string())
                                }));
```

Do this for all three call sites (Ctrl-S body, /retry slash command, Ctrl-T). The dummy channels exist for one commit only — Task 12 wires the real channel through.

- [ ] **Step 3: Build to verify the rename compiles cleanly**

Run: `cd triage-cli-rs && cargo build 2>&1 | tail -10`
Expected: builds successfully.

Run: `cd triage-cli-rs && cargo test --lib 2>&1 | tail -5`
Expected: existing tests still pass.

- [ ] **Step 4: Commit**

```bash
git add triage-cli-rs/src/tui/inbox.rs
git commit -m "refactor(tui/inbox): send_analyst_turn emits ChatEvents via mpsc

Renames send_analyst_turn → send_analyst_turn_with_progress and
threads a tokio mpsc sender through it. The spawned task now emits
AnalystAppended, ProviderRequest, ProviderResponse, TurnPersisted,
and ProviderError events, plus a Phase(Ingesting) opener and the
five phase events emitted by pipeline::followup_turn via the
MpscPhaseReporter. Call sites use a dummy channel temporarily;
the next commit wires the real one.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 12: Wire the mpsc channel + ChatLogger into run_chat_session

**Files:**
- Modify: `triage-cli-rs/src/tui/inbox.rs:1705-2069` (the entire `run_chat_session` body)

This is the largest single mechanical change. Touch only what the spec describes — the rest of `run_chat_session` (input modes, key handling, scroll) stays as-is.

- [ ] **Step 1: Write a failing integration test for the wire-up**

Add a new test module at the bottom of `tui/inbox.rs` (just above the closing `}` of the file's final `#[cfg(test)] mod tests`):

```rust
    // ── run_chat_session mpsc wiring (#22 revamp) ───────────────────

    #[tokio::test]
    async fn event_loop_logs_full_turn_sequence_via_mpsc() {
        // We test the integration of the channel + logger by driving
        // send_analyst_turn_with_progress directly with a recording channel,
        // then assert the JSONL log contains every event in order.
        use crate::chat::{
            chat_events_log_path, ChatEvent, ChatLogger, ChatStage, ChatPhaseReporter,
            MpscPhaseReporter,
        };
        let dir = tempfile::tempdir().unwrap();
        let ticket_dir = dir.path().join("44776");
        std::fs::create_dir_all(crate::chat::session_dir(&ticket_dir)).unwrap();

        struct StubProvider;
        impl crate::providers::LlmProvider for StubProvider {
            fn name(&self) -> &'static str { "stub" }
            fn complete<'a>(
                &'a self, _: &'a str, _: &'a str, _: &'a str,
            ) -> std::pin::Pin<Box<dyn std::future::Future<
                Output = Result<crate::providers::CompletionResult, crate::providers::ProviderError>
            > + Send + 'a>> { Box::pin(async { unreachable!() }) }
            fn followup<'a>(
                &'a self, _sid: Option<&'a str>, _p: &'a str, _sys: &'a str, _m: &'a str,
                _att: &'a [crate::models::Attachment],
            ) -> std::pin::Pin<Box<dyn std::future::Future<
                Output = Result<crate::providers::FollowupResult, crate::providers::ProviderError>
            > + Send + 'a>> {
                Box::pin(async move {
                    Ok(crate::providers::FollowupResult {
                        text: "stub answer".into(),
                        tokens_in: Some(10),
                        tokens_out: Some(20),
                        session_id: Some("01HSTUB".into()),
                        resumed: false,
                    })
                })
            }
        }

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
        let mut logger = ChatLogger::open(&ticket_dir).unwrap();

        // Drive a single turn.
        let provider = StubProvider;
        super::send_analyst_turn_with_progress(
            &ticket_dir,
            "44776",
            "what changed?",
            Vec::new(),
            &provider,
            tx,
        )
        .await
        .unwrap();

        // Drain the channel and log.
        let mut kinds: Vec<&'static str> = Vec::new();
        while let Ok(evt) = rx.try_recv() {
            kinds.push(match &evt {
                ChatEvent::Phase { stage: ChatStage::Ingesting, .. } => "phase:ingesting",
                ChatEvent::Phase { stage: ChatStage::ContextAssembled, .. } => "phase:context",
                ChatEvent::Phase { stage: ChatStage::ProviderAwait, .. } => "phase:await",
                ChatEvent::Phase { stage: ChatStage::ResponseParsed, .. } => "phase:parsed",
                ChatEvent::Phase { stage: ChatStage::Saved, .. } => "phase:saved",
                ChatEvent::AnalystAppended { .. } => "analyst",
                ChatEvent::ProviderRequest { .. } => "req",
                ChatEvent::ProviderResponse { .. } => "resp",
                ChatEvent::TurnPersisted { .. } => "persisted",
                other => panic!("unexpected event: {other:?}"),
            });
            logger.log(&evt);
        }
        drop(logger);

        // The order is what the spec specifies: Ingesting, AnalystAppended,
        // ProviderRequest, then the four pipeline phases (no SessionResumeAttempt
        // since there's no prior codex turn), then ProviderResponse, then
        // TurnPersisted.
        assert_eq!(
            kinds,
            vec![
                "phase:ingesting",
                "analyst",
                "req",
                "phase:context",
                "phase:await",
                "phase:parsed",
                "phase:saved",
                "resp",
                "persisted",
            ]
        );

        let log_body = std::fs::read_to_string(chat_events_log_path(&ticket_dir)).unwrap();
        assert_eq!(log_body.lines().count(), 9, "logger should have written 9 events");
    }
```

Note: the test's expected `kinds` ordering depends on exactly when each event is sent in `send_analyst_turn_with_progress`. If the test fails on ordering, adjust the sequence to match what the implementation actually emits — the spec contract is "events arrive in chronological order"; the exact ordering of `AnalystAppended` vs `Phase(Ingesting)` is a detail.

- [ ] **Step 2: Run test to verify it fails**

Run: `cd triage-cli-rs && cargo test --lib tui::inbox::tests::event_loop_logs 2>&1 | tail -30`
Expected: FAIL (or PASS — if the sequence happens to match exactly). If it fails on ordering, capture the actual sequence and update the test's expected vector to match, then continue.

- [ ] **Step 3: Wire the real mpsc + logger through run_chat_session**

In `triage-cli-rs/src/tui/inbox.rs`, in `run_chat_session` (starting around line 1705):

A. After `let ticket_dir = ticket_folder::tickets_root().join(ticket_id);` (around line 1713), add the channel and logger:

```rust
    let (chat_tx, mut chat_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::chat::ChatEvent>();
    let mut chat_logger = crate::chat::ChatLogger::open(&ticket_dir)
        .unwrap_or_else(|_| {
            // Logger open failure: fall back to a sink that drops events.
            // Open() itself returns Err only if .session dir can't be created
            // — which is the same condition that breaks the conversation
            // lock, so the user already has a worse error to face.
            crate::chat::ChatLogger::open(&ticket_dir).unwrap()
        });

    // Lifecycle event: session opened.
    let _ = chat_tx.send(crate::chat::ChatEvent::SessionOpened {
        ticket_id: ticket_id.to_string(),
        ts: chrono::Utc::now(),
    });
```

B. Inside the `loop {` (around line 1744), at the top of the loop body — before the existing `if let Some(handle) = active_job.as_ref()` block — drain the channel:

```rust
        // Drain pending chat events. Each event is logged and folded
        // through `update_progress` into the running `in_flight` that
        // drives the banner. Terminal events (TurnPersisted /
        // ProviderError / Cancelled) cause `update_progress` to return
        // None, which clears the banner; the join-handle cleanup below
        // takes care of resetting the textarea and status hint when
        // the spawned task actually finishes.
        while let Ok(evt) = chat_rx.try_recv() {
            chat_logger.log(&evt);
            in_flight = crate::chat::update_progress(in_flight.take(), &evt);
            if matches!(
                &evt,
                crate::chat::ChatEvent::TurnPersisted { .. }
                    | crate::chat::ChatEvent::ProviderError { .. }
                    | crate::chat::ChatEvent::Cancelled { .. }
            ) {
                turn_started = None;
            }
        }
```

C. Keep the existing `if let Some(handle) = active_job.as_ref()` block (lines 1745-1781) mostly intact — its job now narrows to: detect that the spawned task has finished, run textarea / status-hint cleanup, and advance the spinner via `advance_progress_tick` between events. Replace the body with:

```rust
        if let Some(handle) = active_job.as_ref() {
            if handle.is_finished() {
                let finished = active_job.take().expect("just checked");
                match finished.await {
                    Ok(Ok(())) => {
                        status_hint = None;
                        clear_textarea(&mut ask_input);
                        transcript_follow_bottom = true;
                    }
                    Ok(Err(msg)) => {
                        let _ = append_chat_system_turn(
                            &ticket_dir,
                            ticket_id,
                            &format!("follow-up failed: {msg}"),
                        );
                        status_hint = Some(msg);
                    }
                    Err(e) => {
                        let m = format!("chat task panicked: {e}");
                        let _ = append_chat_system_turn(&ticket_dir, ticket_id, &m);
                        status_hint = Some(m);
                    }
                }
                turn_started = None;
            } else if let Some(started) = turn_started {
                let elapsed = started.elapsed().as_secs_f64();
                in_flight = in_flight.map(|p| crate::chat::advance_progress_tick(p, elapsed));
            }
        }
```

D. At each spawn call site (the three places that spawn `send_analyst_turn_with_progress`), replace the dummy channel from Task 11 with the real one:

```rust
                                let tx = chat_tx.clone();
                                active_job = Some(tokio::spawn(async move {
                                    send_analyst_turn_with_progress(
                                        &td,
                                        &tid,
                                        &b,
                                        evidence,
                                        provider.as_ref(),
                                        tx,
                                    )
                                    .await
                                    .map_err(|e| e.to_string())
                                }));
```

(Apply the same change at the `/retry` and `Ctrl-T` spawn sites — every spawn that calls `send_analyst_turn_with_progress`.)

E. Inside the key-handling branches, emit `KeyCommand` events for parsed commands. After `let cmd = parse_chat_command(&body);` (around line 1902), add:

```rust
                        let _ = chat_tx.send(crate::chat::ChatEvent::KeyCommand {
                            ts: chrono::Utc::now(),
                            command: match &cmd {
                                ChatCommand::Body(_) => "send".into(),
                                ChatCommand::File(_) => "/file".into(),
                                ChatCommand::Dir { .. } => "/dir".into(),
                                ChatCommand::Paste { .. } => "/paste".into(),
                                ChatCommand::Revise => "/revise".into(),
                                ChatCommand::Retry => "/retry".into(),
                                ChatCommand::Quit => "/quit".into(),
                            },
                        });
```

F. In the Esc-during-inflight branch (around line 1804-1820), emit `Cancelled`:

```rust
                    (KeyCode::Esc, _) => {
                        if let Some(handle) = active_job.take() {
                            handle.abort();
                        }
                        let _ = chat_tx.send(crate::chat::ChatEvent::Cancelled {
                            ts: chrono::Utc::now(),
                            by: crate::chat::CancelSource::EscKey,
                        });
                        in_flight = None;
                        turn_started = None;
                        status_hint = Some("turn cancelled".into());
                    }
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        if let Some(handle) = active_job.take() {
                            handle.abort();
                        }
                        let _ = chat_tx.send(crate::chat::ChatEvent::Cancelled {
                            ts: chrono::Utc::now(),
                            by: crate::chat::CancelSource::CtrlC,
                        });
                        break;
                    }
```

G. Before `Ok(())` at the end of `run_chat_session` (around line 2068), emit `SessionClosed`:

```rust
    let _ = chat_tx.send(crate::chat::ChatEvent::SessionClosed {
        ts: chrono::Utc::now(),
        reason: crate::chat::SessionCloseReason::UserQuit,
    });
    // Drain any pending events one last time so SessionClosed lands in the log.
    while let Ok(evt) = chat_rx.try_recv() {
        chat_logger.log(&evt);
    }
```

Remove the dummy-channel block from Task 11 (`let (dummy_tx, _dummy_rx) = ...`) — the real `chat_tx.clone()` is used everywhere now.

- [ ] **Step 4: Run all tests**

Run: `cd triage-cli-rs && cargo test --lib 2>&1 | tail -30`
Expected: PASS — all existing tests still pass; the new event-loop integration test passes.

Run: `cd triage-cli-rs && cargo build --release 2>&1 | tail -5`
Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add triage-cli-rs/src/tui/inbox.rs
git commit -m "feat(tui/inbox): wire chat mpsc + ChatLogger into run_chat_session

Drains the chat event channel each tick; logs every event to
.session/chat-events.log; folds events through update_progress
into the in_flight banner state. KeyCommand events fire for every
parsed user action. Esc and Ctrl-C cancel paths emit Cancelled
events. SessionOpened/SessionClosed bracket the loop.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 13: Ctrl-D modal + /dir command branch in run_chat_session

**Files:**
- Modify: `triage-cli-rs/src/tui/inbox.rs:1721-1888` (the `ChatInputMode` enum and the key handler for `Ask` / new `DirPath`)

- [ ] **Step 1: Write failing integration test**

Add to the test module at the bottom of `tui/inbox.rs`:

```rust
    #[tokio::test]
    async fn collect_dir_then_log_attached_and_rejected() {
        // Drive collect_dir_attachments + emit EvidenceAttached / Rejected
        // events; assert the JSONL log captures both.
        use crate::chat::{
            chat_events_log_path, ChatEvent, ChatLogger, DirSkipped,
        };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("logs");
        for i in 0..30u32 {
            std::fs::create_dir_all(&src).unwrap();
            std::fs::write(src.join(format!("a{i:03}.log")), "x").unwrap();
        }
        let ticket_dir = dir.path().join("44776");
        std::fs::create_dir_all(crate::chat::session_dir(&ticket_dir)).unwrap();

        let result = crate::chat::collect_dir_attachments(
            &ticket_dir, 1, &src, false, None, 25, 4 << 20,
        )
        .unwrap();
        assert_eq!(result.attached.len(), 25);
        assert_eq!(result.skipped.len(), 5);

        let mut logger = ChatLogger::open(&ticket_dir).unwrap();
        for p in &result.attached {
            logger.log(&ChatEvent::EvidenceAttached {
                ts: chrono::Utc::now(),
                provenance: p.clone(),
            });
        }
        for s in &result.skipped {
            let reason = match s {
                DirSkipped::FileCapExceeded { path } => format!("file_cap: {}", path.display()),
                _ => "other".into(),
            };
            logger.log(&ChatEvent::EvidenceRejected {
                ts: chrono::Utc::now(),
                reason,
            });
        }
        drop(logger);

        let body = std::fs::read_to_string(chat_events_log_path(&ticket_dir)).unwrap();
        let attached_lines = body.lines().filter(|l| l.contains("evidence_attached")).count();
        let rejected_lines = body.lines().filter(|l| l.contains("evidence_rejected")).count();
        assert_eq!(attached_lines, 25);
        assert_eq!(rejected_lines, 5);
    }
```

- [ ] **Step 2: Run test to verify it passes (this exercises Task 8's collect_dir_attachments through the logger from Task 4)**

Run: `cd triage-cli-rs && cargo test --lib tui::inbox::tests::collect_dir_then_log 2>&1 | tail -10`
Expected: PASS — the test only exercises code already in place. (If it fails, an integration issue between collect_dir_attachments and ChatLogger needs fixing.)

- [ ] **Step 3: Add Ctrl-D modal + DirPath input mode**

In `triage-cli-rs/src/tui/inbox.rs`, in `run_chat_session`:

A. Extend the `ChatInputMode` enum (around line 1721) with a `DirPath` variant:

```rust
    enum ChatInputMode {
        Ask,
        FilePath(String),
        PasteLine(String),
        DirPath(String), // new
    }
```

B. Extend the `ChatInputSurface` match (around line 1785) — the input modal needs a render path for `DirPath`. The simplest extension reuses the existing `FilePath` surface (the spec says "modal analogous to the existing FilePath modal"). In `tui/chat.rs`, add a `DirPath` variant to the `ChatInputSurface` enum (around line 28):

```rust
pub enum ChatInputSurface<'a> {
    Ask(&'a TextArea<'a>),
    FilePath { value: &'a str },
    PasteLine { value: &'a str },
    DirPath { value: &'a str }, // new
}
```

And add a render case for it in `render_input` (around line 145, after the `FilePath` branch):

```rust
        ChatInputSurface::DirPath { value } => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(" DIR PATH (Enter attach, Esc cancel; -r for recurse; *.log for glob) ");
            let inner = block.inner(area);
            block.render(area, buf);
            let lines = vec![
                Line::from(""),
                Line::from(vec![Span::raw("dir> "), Span::raw(*value), Span::raw("_")]),
            ];
            Paragraph::new(lines).render(inner, buf);
        }
```

C. Back in `run_chat_session`, extend the `input_surface` match (around line 1785):

```rust
        let input_surface = match &input_mode {
            ChatInputMode::Ask => ChatInputSurface::Ask(&ask_input),
            ChatInputMode::FilePath(v) => ChatInputSurface::FilePath { value: v.as_str() },
            ChatInputMode::PasteLine(v) => ChatInputSurface::PasteLine { value: v.as_str() },
            ChatInputMode::DirPath(v) => ChatInputSurface::DirPath { value: v.as_str() },
        };
```

D. Add a key-handler branch for `ChatInputMode::DirPath(buf)` (after the existing `PasteLine` branch, around line 1873):

```rust
                ChatInputMode::DirPath(buf) => match key.code {
                    KeyCode::Esc => input_mode = ChatInputMode::Ask,
                    KeyCode::Enter => {
                        let raw = buf.trim().to_string();
                        let cmd = parse_chat_command(&format!("/dir {raw}"));
                        if let ChatCommand::Dir { path, recursive, glob } = cmd {
                            match next_turn_number(&ticket_dir).and_then(|turn_no| {
                                crate::chat::collect_dir_attachments(
                                    &ticket_dir,
                                    turn_no,
                                    &path,
                                    recursive,
                                    glob.as_deref(),
                                    25,
                                    4 * 1024 * 1024,
                                )
                                .map_err(|e| anyhow::anyhow!("{e}"))
                            }) {
                                Ok(result) => {
                                    let n_attached = result.attached.len();
                                    let n_skipped = result.skipped.len();
                                    for prov in &result.attached {
                                        let _ = chat_tx.send(
                                            crate::chat::ChatEvent::EvidenceAttached {
                                                ts: chrono::Utc::now(),
                                                provenance: prov.clone(),
                                            },
                                        );
                                        pending_evidence.push(prov.clone());
                                    }
                                    for s in &result.skipped {
                                        let reason = format!("{s:?}");
                                        let _ = chat_tx.send(
                                            crate::chat::ChatEvent::EvidenceRejected {
                                                ts: chrono::Utc::now(),
                                                reason,
                                            },
                                        );
                                    }
                                    let _ = append_chat_system_turn(
                                        &ticket_dir,
                                        ticket_id,
                                        &format!(
                                            "attached {n_attached} file(s) from {}; skipped {n_skipped}.",
                                            path.display()
                                        ),
                                    );
                                }
                                Err(e) => {
                                    let _ = append_chat_system_turn(
                                        &ticket_dir,
                                        ticket_id,
                                        &format!("attach dir failed: {e}"),
                                    );
                                }
                            }
                        }
                        input_mode = ChatInputMode::Ask;
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Char(c) => buf.push(c),
                    _ => {}
                },
```

E. Add a `Ctrl-D` shortcut in the `Ask` branch (next to the existing `Ctrl-F` / `Ctrl-V` shortcuts at line 1884-1888):

```rust
                    (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                        input_mode = ChatInputMode::DirPath(String::new());
                    }
```

F. Update the command bar in `tui/chat.rs` (around line 217-242) to include the new keybinding:

```rust
    let cmds = [
        ("Ctrl-S", "send"),
        ("Ctrl-F", "file"),
        ("Ctrl-D", "dir"),    // new
        ("Ctrl-V", "paste"),
        ("Ctrl-R", "/revise"),
        ("Ctrl-T", "retry"),
        ("Esc", "cancel"),
        ("Ctrl-C", "quit"),
    ];
```

G. Add a parser branch for `ChatCommand::Dir` in the `Ctrl-S` (send) handler — when the analyst types `/dir <path>` in the Ask textarea and hits Ctrl-S, the same `collect_dir_attachments` flow should run. In the `match cmd` block around line 1903, after `ChatCommand::File(path) => { ... }`, add:

```rust
                            ChatCommand::Dir { path, recursive, glob } => {
                                match next_turn_number(&ticket_dir).and_then(|turn_no| {
                                    crate::chat::collect_dir_attachments(
                                        &ticket_dir,
                                        turn_no,
                                        &path,
                                        recursive,
                                        glob.as_deref(),
                                        25,
                                        4 * 1024 * 1024,
                                    )
                                    .map_err(|e| anyhow::anyhow!("{e}"))
                                }) {
                                    Ok(result) => {
                                        let n_attached = result.attached.len();
                                        let n_skipped = result.skipped.len();
                                        for prov in &result.attached {
                                            let _ = chat_tx.send(
                                                crate::chat::ChatEvent::EvidenceAttached {
                                                    ts: chrono::Utc::now(),
                                                    provenance: prov.clone(),
                                                },
                                            );
                                            pending_evidence.push(prov.clone());
                                        }
                                        for s in &result.skipped {
                                            let _ = chat_tx.send(
                                                crate::chat::ChatEvent::EvidenceRejected {
                                                    ts: chrono::Utc::now(),
                                                    reason: format!("{s:?}"),
                                                },
                                            );
                                        }
                                        let _ = append_chat_system_turn(
                                            &ticket_dir,
                                            ticket_id,
                                            &format!(
                                                "attached {n_attached} file(s) from {}; skipped {n_skipped}.",
                                                path.display()
                                            ),
                                        );
                                    }
                                    Err(e) => {
                                        let _ = append_chat_system_turn(
                                            &ticket_dir,
                                            ticket_id,
                                            &format!("attach dir failed: {e}"),
                                        );
                                    }
                                }
                                clear_textarea(&mut ask_input);
                            }
```

- [ ] **Step 4: Run all tests**

Run: `cd triage-cli-rs && cargo test --lib 2>&1 | tail -30`
Expected: PASS — all tests pass; the `collect_dir_then_log_attached_and_rejected` test confirms the wire-up.

Run: `cd triage-cli-rs && cargo build --release 2>&1 | tail -5`
Expected: clean build.

- [ ] **Step 5: Commit**

```bash
git add triage-cli-rs/src/tui/inbox.rs triage-cli-rs/src/tui/chat.rs
git commit -m "feat(tui/inbox): Ctrl-D modal and /dir command for directory attach

Adds ChatInputMode::DirPath + ChatInputSurface::DirPath. Both the
Ctrl-D modal and the /dir slash command from the Ask textarea run
through chat::collect_dir_attachments with 25-file / 4 MiB caps.
Every accepted file emits EvidenceAttached; every skipped file
emits EvidenceRejected. A System turn summarizes the batch so the
analyst sees what was picked up before sending the next question.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 14: Lint + format + smoke-test the binary

**Files:** none modified — verification only

- [ ] **Step 1: Format**

Run: `cd triage-cli-rs && cargo fmt --all`
Expected: zero output (idempotent).

- [ ] **Step 2: Lint**

Run: `cd triage-cli-rs && cargo clippy --all-targets -- -D warnings 2>&1 | tail -30`
Expected: zero warnings, zero errors.

If clippy flags issues, fix them in the file they originate from and re-run. Common likely fixes:
- `dead_code` on `stage_label_str` if not used outside the banner (allow with `#[allow(dead_code)]` only if intentional).
- `useless_format!` on string concatenations.
- `single_match` patterns.

- [ ] **Step 3: Build release binary**

Run: `cd triage-cli-rs && cargo build --release 2>&1 | tail -5`
Expected: release binary built at `target/release/triage-cli`.

- [ ] **Step 4: Smoke-test the doctor subcommand**

Run: `./triage-cli-rs/target/release/triage-cli doctor 2>&1 | tail -20`
Expected: doctor runs to completion (output depends on the local env; the goal is to confirm the binary doesn't panic).

- [ ] **Step 5: Smoke-test that inbox compiles into the binary**

Run: `./triage-cli-rs/target/release/triage-cli inbox --help 2>&1 | head -20`
Expected: clap help text prints; no panic.

- [ ] **Step 6: Commit any formatting / lint fixes**

```bash
git status --short
# If any files changed:
git add -u
git commit -m "chore: cargo fmt + clippy fixes from inbox chat revamp

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

If nothing changed, skip the commit.

---

## Task 15: Update CLAUDE.md / AGENTS.md with the new chat surface

**Files:**
- Modify: `CLAUDE.md` (project root)
- Modify: `AGENTS.md` (project root — kept in sync per the file's own note)

- [ ] **Step 1: Find the Inbox TUI section in CLAUDE.md**

Run: `grep -n "Inbox TUI\|chat pane\|CHAT\|chat-events" /Users/envelazquez/Documents/triage-cli-latest/triage-cli/CLAUDE.md`

Expected: the existing inbox section starts at the line matching "Inbox TUI" near the bottom of the architecture section.

- [ ] **Step 2: Add a "Chat surface" subsection to CLAUDE.md**

Insert under the existing `### Inbox TUI` section (just after the keybindings paragraph) the following new subsection:

```markdown
### Chat surface (progress banner + event log)

Inside the inbox, pressing `a` (open chat for the selected ticket) launches `tui::inbox::run_chat_session`. The chat session runs an mpsc channel — one per session — that the spawned per-turn task posts `chat::ChatEvent` records to (lifecycle, key commands, evidence attaches, phase transitions, provider request/response, errors, cancels). The inbox event loop drains the channel on every 80ms tick: every event is appended to `<ticket_dir>/.session/chat-events.log` as JSON Lines via `chat::ChatLogger`, and every event is folded through `chat::update_progress` into the running `chat::ChatProgress` that drives the banner.

The progress banner above the input box has four responsive tiers — full bordered (≥20 rows), compact bordered (≥14), tight unbordered (≥10), single-row fallback (<10). Layout is computed every draw from `area.height`, so terminal resizes are invisible. `chat::ChatStage` (six variants: Ingesting, ContextAssembled, SessionResumeAttempt, ProviderAwait, ResponseParsed, Saved) maps to the canned status string via `chat::canned_message`; the only stage that rotates strings is `ProviderAwait` (every ~4s, cosmetic — the log records one Phase event for the whole await).

The five phase events come from `pipeline::followup_turn`, which now accepts an optional `&dyn chat::ChatPhaseReporter`. CLI callers (`triage`, `investigate`, `watch`, `pipeline::revise`) pass `None` and behavior is unchanged; only the chat path passes a real `chat::MpscPhaseReporter`.

`Ctrl-D` and the `/dir <path> [-r] [glob]` slash command attach a directory's worth of evidence in one shot — single-level by default with `-r` for recursion, optional `*`/`?` glob filter, hard-capped at 25 files and 4 MiB aggregate. Each accepted file becomes one `EvidenceProvenance` via `chat::attach_file`; each skipped file emits an `EvidenceRejected` event with a reason. A System turn summarizes the batch.

The chat-events log is **per-ticket** (not global) and **never contains prompt body, system prompt, or evidence content** — only counts (`prompt_bytes`), sha256s, paths, and redacted error messages. Caller PII redaction at the LLM boundary (`redact::redact`) still applies before any prompt reaches the provider.
```

- [ ] **Step 3: Mirror the change into AGENTS.md**

CLAUDE.md and AGENTS.md are kept in sync (see the note at the top of CLAUDE.md). Apply the same subsection insertion into AGENTS.md at the equivalent position.

Run: `diff /Users/envelazquez/Documents/triage-cli-latest/triage-cli/CLAUDE.md /Users/envelazquez/Documents/triage-cli-latest/triage-cli/AGENTS.md | head -40`
Expected: minimal differences (just the file-header note pointing to the other agent name).

- [ ] **Step 4: Update the "Where things live" table**

In both CLAUDE.md and AGENTS.md, add these rows to the "Where things live" table (alphabetical sort under the existing entries):

```markdown
| Chat-events log (per-ticket JSONL) | `triage-cli-rs/src/chat.rs` (`ChatLogger`, `chat_events_log_path`); on-disk at `<ticket_dir>/.session/chat-events.log` |
| Chat progress + phase channel | `triage-cli-rs/src/chat.rs` (`ChatEvent`, `ChatStage`, `ChatProgress`, `ChatPhaseReporter`, `MpscPhaseReporter`, `update_progress`, `advance_progress_tick`, `canned_message`) |
| Directory evidence attach | `triage-cli-rs/src/chat.rs` (`collect_dir_attachments`, `DirCollectResult`, `DirSkipped`) |
```

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md AGENTS.md
git commit -m "docs: document inbox chat revamp (progress banner + JSONL log + /dir)

Adds a Chat surface subsection under the Inbox TUI architecture
section in both CLAUDE.md and AGENTS.md, plus three rows in the
'Where things live' table. Spec lives at
docs/superpowers/specs/2026-05-20-inbox-chat-revamp-design.md.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Self-review checklist (run after writing all tasks)

- [x] Every spec section maps to at least one task:
  - Goals (progress / logging / dir attach) → Tasks 1-3, 4, 8/13
  - High-level architecture diagram → Tasks 6, 11, 12
  - Module changes table → Files touched in each task header
  - Data shapes (ChatEvent, ChatStage, ChatProgress, SessionCloseReason, CancelSource) → Tasks 1, 2, 3
  - ChatPhaseReporter trait + MpscPhaseReporter → Task 5
  - ChatLogger + chat_events_log_path → Task 4
  - Phase boundaries (5 emission sites) → Task 6
  - Canned-message rotation → Tasks 1, 3
  - Banner — responsive layout (4 tiers) → Task 10
  - Color per stage → Task 10 (stage_color)
  - Resize handling → Task 10 (banner_rows clamps per draw)
  - Directory attachment (/dir, Ctrl-D, collect_dir_attachments, DirSkipped, DirCollectResult) → Tasks 7, 8, 9, 13
  - Event-loop refactor (mpsc replaces is_finished polling) → Task 12
  - Cancel discipline (Esc + Ctrl-C emit Cancelled, dedupe by ts) → Task 12
  - PII boundary (prompt_bytes not body; redacted ProviderError.message) → Tasks 2, 11
  - Tests at each layer → woven into every task
  - Migration (InFlightState → ChatProgress) → Task 10
  - Out of scope items (streaming, file picker, $EDITOR, etc.) → explicitly NOT added; no task creates them
  - Estimated PR shape → Total estimate matches: ~650 lines across 4 files
- [x] No placeholders, no TBD, no "implement later".
- [x] Every code step shows complete code, not a sketch.
- [x] Type / function signatures consistent: `ChatStage`, `ChatEvent`, `ChatProgress`, `ChatLogger`, `ChatPhaseReporter`, `MpscPhaseReporter`, `update_progress`, `advance_progress_tick`, `canned_message`, `chat_events_log_path`, `collect_dir_attachments`, `simple_glob_match`, `DirCollectResult`, `DirSkipped` all match across tasks.
- [x] `pipeline::followup_turn` signature change (Task 6) is honored by every later call site update (Task 11).
- [x] `InFlightState` → `ChatProgress` migration (Task 10) updates every reference in `inbox.rs`.
- [x] Each task ends with a commit so the work is bisectable.
- [x] Existing tests continue to pass after each task (verified by `cargo test --lib` step in tasks 6, 10, 12, 13).

---

## Done criteria

All 15 tasks committed in order on branch `codex/remove-investigate-save-flag` (or a fresh feature branch — confirm with the user before pushing). The acceptance signal:

1. `cd triage-cli-rs && cargo test --lib && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` exits 0.
2. `target/release/triage-cli inbox` (with a populated `Tickets/` directory) opens, pressing `a` opens the chat pane, pressing `Ctrl-S` after typing a message shows the responsive banner with a rotating canned message under `ProviderAwait`, and `<ticket_dir>/.session/chat-events.log` accumulates one JSONL line per event.
3. `Ctrl-D` opens the dir-path modal, entering a directory path attaches up to 25 files with a System turn summarizing the batch.
