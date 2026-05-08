// Package assessment defines the Assessor interface and a deterministic
// stub implementation used when no LLM-backed assessor is available.
package assessment

import (
	"context"
	"fmt"
	"strings"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// Assessor turns an in-flight investigation session into a structured
// Assessment. Implementations may be deterministic stubs or LLM-backed.
type Assessor interface {
	Assess(ctx context.Context, session model.InvestigationSession) (model.Assessment, error)
}

// StubAssessor produces a deterministic, honest assessment from the
// session contents alone. It never claims Confirmed or Likely confidence
// because it does not actually analyze content — that is reserved for
// future LLM-backed assessors.
type StubAssessor struct{}

// Assess returns a deterministic assessment based on evidence count
// and shape. No fabrication: when evidence is thin, confidence is
// explicitly Unknown.
func (StubAssessor) Assess(_ context.Context, session model.InvestigationSession) (model.Assessment, error) {
	summary := firstSentence(session.Ticket.Subject)
	if desc := firstSentence(session.Ticket.Description); desc != "" {
		summary = fmt.Sprintf("%s — %s", summary, desc)
	}

	correlation := buildCorrelation(session.Evidence)
	unknowns := buildUnknowns(session.Evidence)
	nextSteps := []string{
		"Confirm reproduction with reporter",
		"Collect station-level logs from affected workstation",
		"Cross-reference with recent platform changes",
	}

	count := len(session.Evidence)
	var rootCause string
	var confidence model.Confidence
	switch {
	case count <= 1:
		rootCause = "Insufficient evidence to identify a likely root cause."
		confidence = model.ConfidenceUnknown
		nextSteps = append(nextSteps, "Request additional evidence from reporter (logs, timestamps, affected workstation)")
	case count <= 4:
		rootCause = "See correlation; pattern needs corroboration before a root cause can be claimed."
		confidence = model.ConfidencePossible
	default:
		rootCause = "See correlation; multiple evidence sources reviewed but stub assessor does not infer specific causes."
		confidence = model.ConfidencePossible
	}

	internalNote := buildInternalNote(session.Ticket, session.Evidence, confidence)

	return model.Assessment{
		Summary:               summary,
		LikelyRootCause:       rootCause,
		Confidence:            confidence,
		Correlation:           correlation,
		Unknowns:              unknowns,
		NextSteps:             nextSteps,
		SuggestedInternalNote: internalNote,
	}, nil
}

func buildCorrelation(ev []model.Evidence) []string {
	if len(ev) == 0 {
		return []string{"No evidence collected yet."}
	}
	counts := map[model.EvidenceKind]int{}
	var lineTotal int
	for _, e := range ev {
		counts[e.Kind]++
		lineTotal += e.LineCount
	}
	out := make([]string, 0, len(counts))
	if n := counts[model.EvidenceKindComment]; n > 0 {
		out = append(out, fmt.Sprintf("%d ticket comment(s) reviewed", n))
	}
	if n := counts[model.EvidenceKindAttachment]; n > 0 {
		out = append(out, fmt.Sprintf("%d attachment reference(s) noted (not downloaded in spike)", n))
	}
	if n := counts[model.EvidenceKindLocalFile]; n > 0 {
		out = append(out, fmt.Sprintf("%d local log file(s) ingested (%d lines total)", n, lineTotal))
	}
	if n := counts[model.EvidenceKindPaste]; n > 0 {
		out = append(out, fmt.Sprintf("%d pasted text block(s) ingested", n))
	}
	return out
}

func buildUnknowns(ev []model.Evidence) []string {
	out := []string{
		"Live system logs not queried",
		"Reproduction steps not verified",
	}
	has := map[model.EvidenceKind]bool{}
	for _, e := range ev {
		has[e.Kind] = true
	}
	if !has[model.EvidenceKindLocalFile] {
		out = append(out, "No local log files supplied")
	}
	if !has[model.EvidenceKindAttachment] {
		out = append(out, "No attachments reviewed")
	}
	return out
}

func buildInternalNote(t model.Ticket, ev []model.Evidence, conf model.Confidence) string {
	var b strings.Builder
	fmt.Fprintf(&b, "Triage notes for ZD-%d (%s).\n\n", t.ID, strings.TrimSpace(t.Subject))
	fmt.Fprintf(&b, "Reviewed %d evidence item(s): ", len(ev))
	if len(ev) == 0 {
		b.WriteString("none yet.")
	} else {
		kinds := map[model.EvidenceKind]int{}
		for _, e := range ev {
			kinds[e.Kind]++
		}
		parts := []string{}
		for _, k := range []model.EvidenceKind{
			model.EvidenceKindComment,
			model.EvidenceKindAttachment,
			model.EvidenceKindLocalFile,
			model.EvidenceKindPaste,
		} {
			if n := kinds[k]; n > 0 {
				parts = append(parts, fmt.Sprintf("%d %s", n, k))
			}
		}
		b.WriteString(strings.Join(parts, ", "))
		b.WriteString(".")
	}
	b.WriteString("\n\n")
	if conf == model.ConfidenceUnknown {
		b.WriteString("Confidence is unknown — evidence is thin. Requesting additional logs and reproduction steps from the reporter before drawing conclusions.")
	} else {
		b.WriteString("Confidence is at most 'possible' — patterns surfaced in correlation need corroboration with platform-side logs before a root cause is asserted. No fabricated conclusions in this draft.")
	}
	return b.String()
}

// firstSentence returns the leading sentence of s (up to the first
// `.`, `!`, or `?`), trimmed. If no terminator is found, the full
// trimmed string is returned.
func firstSentence(s string) string {
	s = strings.TrimSpace(s)
	if s == "" {
		return ""
	}
	idx := strings.IndexAny(s, ".!?")
	if idx < 0 {
		return s
	}
	return strings.TrimSpace(s[:idx+1])
}
