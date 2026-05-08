package assessment

import (
	"fmt"
	"strings"
	"time"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// systemInstruction is the role/voice header. Kept separate so future
// callers can supply it via --system rather than embedding inline.
const systemInstruction = `You are a senior NOC engineer triaging a Zendesk ticket for the Carbyne APEX NG911/E911 platform. Produce a strict JSON object matching the schema below. Be honest about uncertainty: if evidence is thin, set confidence to "unknown" and say what's missing in "unknowns". Never fabricate root causes, log lines, or error codes.`

// schemaBlock is the output schema embedded literally in the prompt.
// Field names match model.Assessment JSON tags.
const schemaBlock = `Required JSON schema:

{
  "summary":                "<1-2 sentences. What is the ticket about, in your own words.>",
  "likely_root_cause":      "<1-3 sentences. The most plausible cause given the evidence. If evidence is thin, say so.>",
  "confidence":             "confirmed" | "likely" | "possible" | "unknown",
  "correlation":            ["<short bullet linking a symptom to evidence>", ...],
  "unknowns":               ["<a question the current evidence does not answer>", ...],
  "next_steps":             ["<a concrete verification or remediation step>", ...],
  "suggested_internal_note": "<paste-ready Zendesk internal note. Markdown allowed. Hedge on uncertain claims.>"
}

Confidence calibration:
- "confirmed": evidence directly proves the root cause (e.g., explicit error in logs matching ticket).
- "likely":    evidence strongly suggests one cause without proving it.
- "possible":  evidence is consistent with one or more causes; corroboration needed.
- "unknown":   evidence is absent, ambiguous, or contradicts the ticket.

Example for thin evidence:

{
  "summary": "Reporter says audio drops on the dispatch console at Site X.",
  "likely_root_cause": "Insufficient evidence to identify a root cause. Reporter has not supplied logs or timestamps and the ticket has no attachments.",
  "confidence": "unknown",
  "correlation": ["Subject and description both reference audio dropping; no log evidence yet."],
  "unknowns": ["No timestamps for the affected calls", "No station-level logs from the dispatch workstation"],
  "next_steps": ["Request affected-call timestamps from reporter", "Pull station logs from the affected workstation"],
  "suggested_internal_note": "Triage notes: Reporter reports audio drops on dispatch console at Site X. Awaiting timestamps and station logs before drawing conclusions."
}`

// closingInstruction tells the model to emit only JSON. We still defend
// against fences and prose in the parser.
const closingInstruction = `Output ONLY the JSON object. Do not wrap it in Markdown fences. Do not include any text before or after the JSON.`

// BuildPrompt constructs the full single-turn prompt sent to the
// claude CLI. It is pure: no I/O, deterministic given the same input.
func BuildPrompt(session model.InvestigationSession) string {
	var b strings.Builder
	b.WriteString(systemInstruction)
	b.WriteString("\n\n")
	b.WriteString(schemaBlock)
	b.WriteString("\n\n")
	b.WriteString("=== TICKET ===\n")
	writeTicketBlock(&b, session.Ticket)
	b.WriteString("\n=== EVIDENCE ===\n")
	writeEvidenceBlock(&b, session.Evidence)
	b.WriteString("\n=== TIMELINE ===\n")
	writeTimelineBlock(&b, session.Timeline)
	b.WriteString("\n")
	b.WriteString(closingInstruction)
	return b.String()
}

func writeTicketBlock(b *strings.Builder, t model.Ticket) {
	fmt.Fprintf(b, "ID: ZD-%d\n", t.ID)
	if t.Subject != "" {
		fmt.Fprintf(b, "Subject: %s\n", t.Subject)
	}
	if t.RequesterOrg != "" {
		fmt.Fprintf(b, "Requester org: %s\n", t.RequesterOrg)
	}
	if t.Status != "" {
		fmt.Fprintf(b, "Status: %s\n", t.Status)
	}
	if t.Priority != "" {
		fmt.Fprintf(b, "Priority: %s\n", t.Priority)
	}
	if !t.CreatedAt.IsZero() {
		fmt.Fprintf(b, "Created: %s\n", t.CreatedAt.UTC().Format(time.RFC3339))
	}
	if t.Description != "" {
		b.WriteString("\nDescription:\n")
		b.WriteString(strings.TrimSpace(t.Description))
		b.WriteString("\n")
	}
	if len(t.Comments) > 0 {
		b.WriteString("\nComments:\n")
		for i, c := range t.Comments {
			visibility := "public"
			if !c.Public {
				visibility = "internal"
			}
			ts := ""
			if !c.CreatedAt.IsZero() {
				ts = c.CreatedAt.UTC().Format(time.RFC3339)
			}
			fmt.Fprintf(b, "[%d] %s | %s | %s\n", i+1, visibility, c.AuthorName, ts)
			b.WriteString(strings.TrimSpace(c.Body))
			b.WriteString("\n")
		}
	}
	if len(t.AttachmentRefs) > 0 {
		b.WriteString("\nAttachments:\n")
		for _, a := range t.AttachmentRefs {
			fmt.Fprintf(b, "- %s (%s, %d bytes)\n", a.Filename, a.ContentType, a.SizeBytes)
		}
	}
}

func writeEvidenceBlock(b *strings.Builder, ev []model.Evidence) {
	if len(ev) == 0 {
		b.WriteString("(no evidence collected yet)\n")
		return
	}
	for i, e := range ev {
		label := e.Label
		if label == "" {
			label = e.Source
		}
		fmt.Fprintf(b, "[%d] %s | %s\n", i+1, e.Kind, label)
		if e.Excerpt != "" {
			excerpt := strings.TrimSpace(e.Excerpt)
			b.WriteString(excerpt)
			b.WriteString("\n")
		}
	}
}

func writeTimelineBlock(b *strings.Builder, tl []model.TimelineEvent) {
	if len(tl) == 0 {
		b.WriteString("(no timeline events)\n")
		return
	}
	for _, t := range tl {
		ts := "        "
		if t.Timestamp != nil {
			ts = t.Timestamp.UTC().Format("15:04:05")
		}
		fmt.Fprintf(b, "%s  %s  %s  %s\n", ts, t.Source, t.Kind, oneLine(t.Message))
	}
}

func oneLine(s string) string {
	s = strings.ReplaceAll(s, "\n", " ")
	s = strings.ReplaceAll(s, "\r", " ")
	return strings.TrimSpace(s)
}
