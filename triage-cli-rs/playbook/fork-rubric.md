---
rubric_version: "2026-05-13"
source: "DailyNOC/_triage_pipeline/fork-rubric.md (session-export-2026-05-07.md, sessions 1-7)"
maintained_by: "Carbyne NOC team"
---

# NOC Triage → Fork Rubric

**Purpose:** For any Zendesk ticket, decide as fast as possible which fork it goes to:

- **(a) Engineering Jira** — defect in Carbyne-controlled code or infra (SBC, Kamailio, FreeSWITCH, APEX station client, SDK, translation pipeline, control center).
- **(b) Vendor / Internal IT** — defect or instability in carrier, customer ISP, customer LAN/switch, Masergy/SDWAN, or PSTN carrier.
- **(c) NOC self-resolve** — configuration error, customer training/UX, working-as-designed; close with a customer-facing note.
- **(d) Cannot fork yet** — required evidence is missing; ask for it and pause.

**Stop rule:** Once the fork is unambiguous, stop investigating. Detailed root cause is the new owner's job. Aim for *just enough* evidence to commit to (a), (b), (c), or (d).

---

## Step 0 — Always-on intake (every ticket)

Before symptom-class triage, pull these in parallel. Each line is a *binary* prerequisite for forking.

| Need | Where | If missing |
|---|---|---|
| Customer + site (CNC, friendly name, region) | Zendesk ticket fields → `apex-cnc-inventory.md` | Ask in ticket; do not proceed |
| Incident timestamp (UTC and local) | Ticket body | Ask in ticket |
| Affected station ID(s) / agent | Ticket body | Ask in ticket |
| Last 3 tickets for this customer | `zendesk-mcp__search_tickets` | — |
| Open master ticket for this site/region | `search_tickets` w/ "master" tag or recent pending | — |
| Current deployment version | Confluence release notes / customer field | — |
| Active engineering Jira matching keywords | `search_jira_issues` | — |
| Log/PCAP coverage of incident window ±30 min | `Read` + `Bash grep` on uploaded files | **Request fresh logs; pause triage** |

> If a known master ticket or open Jira already covers this symptom and site, **stop. Fork = (a) add evidence to existing Jira**, or (b) link to master, depending on the prior owner.

---

## Symptom Class 1 — Media loss / audio quality

*Example: TWT degradation, one-way audio, missing greeting, choppy audio, dropped audio mid-call.*

### Required evidence
- PCAP covering call lifecycle (SIP + RTP)
- Station logs covering incident timestamp
- Customer reported time, call ID, agent
- Recent translation pipeline Jiras (e.g., REP-85877 class)

### Fork signals

| Observation | Fork |
|---|---|
| RTP present, end-to-end timestamps healthy, but progressive latency increase | **(a) Engineering** — translation pipeline / buffering upstream of SBC |
| RTP absent in PCAP and signaling shows successful 200 OK + ACK | **(a) Engineering** — media not reaching SBC; SDP/relay issue |
| RTP present and clean, station-side renderer hang or heap spike at incident time | **(a) Engineering** — station client team |
| RTP gaps correlate to caller-side network instability (jitter spikes from caller IP only) | **(b) Vendor** — carrier / caller signal |
| Audio capture stops but call continues (orphaned recording) | **(a) Engineering** — call-leg attribution / recording channel bug |
| STUN keepalive warnings present on **every** call across log set (chronic baseline) | Not a root cause — exclude as signal; keep digging |
| Greeting missing post-recovery from a `RECONNECT_ON_DRAINING` event | **(a) Engineering** — race between `EXTENSION_READY` and greeting engine |

### Stop conditions
- Log window does not cover incident — request server-side FreeSWITCH/Kamailio logs and pause.
- Pattern matches an open REP-class Jira — add evidence, do not investigate further.

---

## Symptom Class 2 — Call routing / wrong PSAP / wrong agent

*Example: 911 lands in wrong jurisdiction; call attributed to wrong agent; missing inbound event.*

### Required evidence
- Inbound SIP INVITE from carrier with To/From headers
- Routing decision logs (Kamailio / dispatcher)
- Recent deployment version (regression candidate)
- Carbyne Event PDFs for the affected call(s)

### Fork signals

| Observation | Fork |
|---|---|
| Carrier sends correct INVITE; our routing chose wrong PSAP | **(a) Engineering** — routing regression; check deployment changeset |
| Carrier INVITE has wrong destination data (bad ANI/ALI from carrier) | **(b) Vendor** — carrier (Verizon, AT&T, etc.) |
| Two near-simultaneous calls and only one displayed/attributed correctly | **(a) Engineering** — call-leg attribution under concurrency |
| Inbound 911 event is missing from records entirely | **(a) Engineering — patient safety priority** — escalate same-day |
| Outbound callback recorded but no inbound event for the same number | **(a) Engineering** — record persistence / orphan event |

### Stop conditions
- Symptom appears in regression from a recent (last 14 days) release → fork (a) immediately with deploy version + Jira reference.
- Pure carrier-side malformed signaling → fork (b), forward PCAP to vendor team.

---

## Symptom Class 3 — Network error banner / WebSocket disconnect / station drops

*Example: "Network Error" banner on station, station status flips to ERROR, brief unavailability.*

### Required evidence
- Kamailio drain logs around incident time (look for `X-Web-Socket-Draining: true`)
- Station logs for `RECONNECT_ON_DRAINING`, code 1006 close, status transitions
- Customer network state (NTT, BGP/FG status, switch logs if available)
- Master ticket lookup for the site

### Fork signals

| Observation | Fork |
|---|---|
| Isolated to single station; no customer-network correlation; Kamailio drain present | **(a) Engineering** — egress node drain anomaly |
| Multiple stations at same site flip ERROR within seconds of each other | **(b) Vendor / IT** — customer LAN, switch, or SDWAN. Link to site master ticket |
| Recurring pattern at same site with open master ticket (e.g., Cobb 41675) | **(b) Vendor / IT** — link to master; do not re-investigate |
| Drain coincides with planned rolling restart (release ops calendar) | **(c) Self-resolve** — customer note: expected maintenance event |
| Greeting missing post-recovery (overlaps Class 1) | Cross-list to Class 1 fork (a) for race condition |

### Stop conditions
- Open master ticket for same site/window exists → **fork (b), link only.**
- Drain originated from a known-flapping egress node already under engineering investigation → **fork (a), add evidence.**

---

## Symptom Class 4 — Dial failures / outbound

*Example: "Destination not reachable", calls drop at N seconds, third attempt succeeds.*

### Required evidence
- PCAPs of failed and (if available) successful attempts to same number
- BYE direction analysis (who sent BYE first)
- Speed dial / config audit for the affected number
- Customer's original complaint wording (verify number is the *complained-about* number, not a different call)

### Fork signals

| Observation | Fork |
|---|---|
| SBC sends unsolicited BYE N seconds post-200 OK consistently | **(a) Engineering** — SBC instability; capture node IP (e.g., `10.4.10.103`) |
| SBC returns SIP 5xx (500/503) | **(a) Engineering** — SBC error response path |
| Carrier returns SIP 4xx (404, 408, 487) cleanly | **(b) Vendor** — carrier rejected; provide PCAP |
| Number dialed does not exist in NANP (e.g., area code 875) | **(c) Self-resolve** — speed dial misconfiguration; customer-facing fix |
| Number never appears in any log → wrong target | **(c) Self-resolve** — verify complaint refers to right call/number |
| Bridge contention from concurrent long calls on same fsconf node | **(a) Engineering** — bridge sizing / contention |

### Stop conditions
- Misconfigured speed dial proven (number invalid or recently changed) → **fork (c)**, customer note with corrected number; close.
- Wrong call investigated (PDFs show successful calls; complaint is about *other* numbers) → reset; pull complaint-target call data before continuing.

---

## Symptom Class 5 — Priority / queue / call-offering timing

*Example: Admin call appears to take priority over 911; "system error" claim from customer.*

### Required evidence
- SBC arrival timestamps for all calls in the window (ms precision)
- Queue / offer logic state at the relevant ms tick
- Priority configuration for the customer

### Fork signals

| Observation | Fork |
|---|---|
| 911 arrived at SBC *after* admin call had been offered to agent (even by ms) | **(c) Self-resolve** — working as designed; customer-facing note explaining offering vs. arrival |
| 911 arrived first at SBC but admin was offered first | **(a) Engineering** — priority logic defect |
| Customer's priority configuration assigns wrong weight to call types | **(c) Self-resolve** — config audit + customer note |

### Stop conditions
- Working-as-designed confirmed by SBC timestamps → fork (c) in single response; do not gather further evidence.

---

## Symptom Class 6 — Data / analytics / event-stream gaps

*Example: Calls missing from analytics dashboard, event counts mismatch, customer reports.*

### Required evidence
- Date range of missing data
- Customer count affected (cluster check)
- Pipeline component (intake vs. enrichment vs. dashboard)

### Fork signals

| Observation | Fork |
|---|---|
| Same gap across 2+ customers in same window | **(a) Engineering** — pipeline-wide; cluster ticket |
| Gap isolated to one customer + correlates with their LAN/VPN issue | **(b) Vendor / IT** |
| Customer's filters or dashboard config excluding records | **(c) Self-resolve** — config training |

### Stop conditions
- ≥2 customers affected same window → **fork (a) as a cluster**; consolidate tickets onto one Jira.

---

## Cross-cutting modifiers

These adjust the fork after symptom-class analysis:

- **Patient safety (missing 911 record, mis-routed 911):** Escalate fork (a) to same-day priority regardless of other factors.
- **Recurring at same site (3+ tickets in 30 days):** Force a master-ticket linkage even if individual fork is (a) or (b).
- **Within 14 days of a release:** Fork (a) candidates get tagged "regression" and the release Jira is referenced.
- **Log gap covers incident:** Cannot fork yet (fork **d**). Request server-side logs, set Zendesk status appropriately, pause.

---

## Output template (paste into Zendesk internal note)

> **Note:** This template is the human-readable form pasted into Zendesk. The CLI emits a structured `TriageReport` JSON object (see `triage-cli-rs/src/models.rs`); the contents below are rendered into `FORK_PACKET.md` and the internal-note draft in `DRAFTS.md`.

```
Fork: (a) Engineering | (b) Vendor/IT | (c) Self-resolve | (d) Cannot fork yet     [pick one]

Symptom class: [1–6]
Customer / site: [name] / [CNC] / [friendly]
Incident time: [UTC] / [local]
Affected: [stations/agents/calls]

Evidence summary:
- [3–5 bullets, observable facts only]

Decision signal triggered:
- [quote the rubric line that committed the fork]

Next action:
- (a) Open Jira [title] / link to [existing Jira]
- (b) Hand off to [vendor / IT team] with [PCAP / log bundle]
- (c) Customer note drafted (see below)
- (d) Request [missing evidence]; pause triage
```

---

## Maintenance

Update this file when:
- A new symptom class appears 3+ times (add Class 7+).
- A fork signal misroutes a ticket (correct the row; note date).
- A vendor or engineering team's intake expectations change (update "next action").

PRs to this file are reviewed like code. When updating, bump `rubric_version` in the frontmatter to today's date.

Last revised: 2026-05-13. Source sessions: `DailyNOC/exports/session-export-2026-05-07.md` (sessions 1–7).
