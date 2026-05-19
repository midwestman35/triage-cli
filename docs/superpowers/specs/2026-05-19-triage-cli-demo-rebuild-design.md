# triage-cli demo rebuild design

Date: 2026-05-19
Status: approved direction, awaiting implementation plan
Target repo for implementation: `midwestman35/triage-cli-video`
Related product repo: `midwestman35/triage-cli`

## Purpose

Rebuild the current Remotion demo as a silent, 3-4 minute, one-ticket story that works for two audiences:

- Internal NOC/team viewers who need to trust the workflow before trying it.
- Portfolio/hiring viewers who need to see taste, systems thinking, and careful product judgment.

The core message is: `triage-cli` is calm automation for high-trust operator workflows. It can assist routing decisions, but it does not guess and it does not autonomously post to audited systems.

## Approved Direction

Use Approach 1: story-first rebuild.

The existing demo's substance should be recreated, but not preserved as a four-scene feature tour. The new flagship cut follows a single ticket from initial support-ticket glance through investigation, evidence addition, fork resolution, generated artifacts, and scale proof.

This should replace the rendered demo's current setup -> investigate -> watch -> inbox structure with a more memorable arc:

1. Generic support ticket opens the story.
2. First `triage-cli investigate` pass produces Fork D because evidence is incomplete.
3. Analyst adds evidence and types a chat/TUI observation.
4. Combined evidence reveals a specific Operator Client render regression.
5. Reprocess resolves the ticket to Fork A.
6. Five generated Markdown files cascade as the main payoff.
7. Inbox/watch appears briefly as proof the same contract scales.

## Runtime and Format

Target runtime: 3-4 minutes.

Primary format: silent-first video. It should be readable while the presenter narrates live, and coherent if watched without narration. Music may be added later, but the visuals should not depend on audio cues.

Do not over-caption. Use chapter cards, short on-screen labels, and highlighted artifact lines. Let the presenter supply the spoken details.

## Story Beats

### 1. The Ticket

Open with a generic but Zendesk-adjacent support ticket in a browser frame. It should be close enough to Zendesk's information architecture for internal viewers to recognize the workflow, but not use real Zendesk branding or sensitive production data.

The ticket describes an Operator Client rendering issue. Visible evidence is incomplete: enough to justify investigation, not enough to route.

### 2. Initial Investigation

Move from browser to terminal. The analyst runs `triage-cli investigate <ticket-id>`.

The terminal should show the recognizable pipeline phases:

- Zendesk ticket fetch
- customer history
- memory lookup
- evidence intake
- optional Datadog or local evidence processing
- structured LLM assessment
- save to ticket folder

The first run lands on Fork D: cannot fork yet. The decisive evidence is missing, and the tool should surface that clearly rather than making a premature routing decision.

### 3. Evidence Added

Show the analyst adding evidence through a visual cursor/evidence moment, then typing an observation into a chat/TUI-style input.

Primary evidence idea:

- A log/screenshot bundle or supplemental artifact indicates an Operator Client build-specific rendering issue.

Typed analyst observation:

- The analyst notes a reproducible render regression in a specific Operator Client build.

The key teaching point: human context plus tool-ingested evidence changes the decision.

### 4. Fork Resolution

The tool reprocesses the ticket. The result changes from Fork D to Fork A: Engineering Jira.

Root cause: a specific Operator Client render regression.

The terminal should make the before/after legible without requiring long reading:

- D: missing decisive evidence
- A: engineering-owned client render regression
- confidence rises
- quoted rubric row appears
- Jira/customer/internal drafts become available behind review gates

### 5. Five-File Markdown Cascade

This is the main visual payoff. Show the generated ticket folder as a hybrid editor/document view: editor tabs preserve actual Markdown-file fidelity, while rendered callouts highlight the key line in each file.

Show all five files with one highlight each:

- `INTAKE.md`: fingerprint / summary establishes the ticket shape.
- `EVIDENCE_PREFLIGHT.md`: missing evidence becomes decisive evidence.
- `FORK_PACKET.md`: Fork A recommendation, confidence, rubric row.
- `DRAFTS.md`: Engineering Jira draft and notes are prepared behind CONFIRM gates.
- `STATE.md`: machine-readable fork/status captures the resolved state.

The cascade should be tight. Do not ask viewers to read full documents.

### 6. Scale Proof

Close the workflow story with a short inbox/watch proof. This should not become a second product tour.

Show several tickets in an inbox/watch view, with the resolved ticket selected. The point is that every ticket lands in the same artifact contract, making review and handoff predictable.

## Visual System

Directly use Motion Studio assets/libraries from `theexperiencecompany/motion-studio` where they fit the project. This is for personal/internal, non-sale use. Keep a short attribution/usage note in the video repo so future readers know where the assets came from and why they are present.

Use the following Motion Studio components or adapted local integrations:

- `TextSoftBlurIn`: chapter cards such as `The Ticket`, `Missing Evidence`, `Fork A`, and `The Handoff`.
- `BrowserWindow`: the generic support-ticket browser frame.
- `CursorWalkthrough`: browser and evidence interactions, including selecting/highlighting supplemental evidence.
- `Terminal`: visual polish reference or direct basis for terminal chrome where compatible.
- `TypingComposer`: basis for the analyst observation input, visually integrated with the CLI/TUI rather than shown as a standalone consumer chat app.
- `Toast`: optional short state notifications, such as evidence processed or fork resolved.

The existing `triage-cli-video` terminal choreography should not be discarded if it contains richer behavior than the Motion Studio terminal component. Preserve or port these behaviors:

- spinners
- phase completion lines
- streaming Markdown
- scroll anchoring
- inbox panes
- file tab transitions

Visual language:

- black terminal base, not theatrical hacker styling
- off-white document surfaces for generated Markdown
- cyan for active system state
- green for verified completion
- yellow for missing evidence or review gates
- red only for true errors
- precise motion: blur-in, spring-in, type, highlight, resolve

Avoid decorative excess. The work should feel calm, operational, and deliberate.

## Component Plan

The Remotion implementation should introduce or refactor around these local scene components:

- `ChapterCard`
- `SupportTicketBrowser`
- `CursorGuide`
- `TriageTerminal`
- `AnalystObservationInput`
- `MarkdownCascade`
- `ScaleProof`
- `FlagshipDemoV2`

These components should be reusable enough to render companion clips later.

## Companion Clips

After the flagship rebuild, create standalone renders from the same scene library:

- `Fork D to Fork A`: the trust story in 60-90 seconds.
- `Five Markdown Files`: artifact contract explainer.
- `Inbox Scale`: how tickets become a reviewable queue.
- `Safe Handoff`: CONFIRM-gated drafts and no autonomous posting.
- `Setup/Doctor`: quick internal onboarding clip.
- `Soft Lock / Rubric Drift`: reviewer/lead confidence clip.

These are not part of the first implementation unless cheap to expose once the flagship composition is working.

## Implementation Constraints

- Preserve the project as a Remotion video.
- Build the flagship cut first.
- Keep the video silent-first.
- Use generic/redacted ticket content.
- Do not show sensitive Zendesk, Datadog, customer, or employee data.
- Prefer real-looking UI structure over brand-accurate proprietary screenshots.
- Do not let Motion Studio components erase the CLI-bound nature of the tool.
- Keep the demo legible at 1920x1080.

## Verification Expectations

Before considering the implementation done:

- Run the Remotion lint/typecheck command available in the video repo.
- Render or preview the flagship composition.
- Verify the video is readable without narration.
- Verify chapter cards, browser scene, terminal text, observation input, Markdown cascade, and inbox/watch proof all render without overlap.
- Verify no sensitive real customer data appears in the demo.

## Open Notes For Implementation

- The local shell was unavailable during brainstorming, so this spec was written from GitHub-read context and user-approved direction.
- Confirm the exact Motion Studio import/copy strategy during implementation based on the `triage-cli-video` dependency setup.
- If direct dependency installation is heavy or incompatible, copy only the approved component assets needed for this video and preserve attribution.
