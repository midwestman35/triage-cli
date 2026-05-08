// Package render formats triage reports for stdout (Markdown / JSON)
// and provides a small stderr status helper.
package render

import (
	"fmt"
	"strings"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// MarkdownOpts controls optional sections of the rendered Markdown.
type MarkdownOpts struct {
	IncludeTimeline bool
}

// Markdown renders a TriageReport as a Markdown document. Section
// headers match the format consumed by the spike acceptance test.
func Markdown(report model.TriageReport, opts MarkdownOpts) string {
	var b strings.Builder

	fmt.Fprintf(&b, "# Triage Report — ZD-%d\n\n", report.TicketID)
	fmt.Fprintf(&b, "_Generated %s by %s_\n\n", report.GeneratedAt.UTC().Format("2006-01-02T15:04:05Z07:00"), report.Tool)

	b.WriteString("## Initial Summary\n")
	b.WriteString(report.Assessment.Summary)
	b.WriteString("\n\n")

	b.WriteString("## Evidence Reviewed\n")
	if len(report.Evidence) == 0 {
		b.WriteString("- (none)\n")
	} else {
		for _, e := range report.Evidence {
			label := e.Label
			if label == "" {
				label = e.Source
			}
			line := fmt.Sprintf("- %s: %s", e.Kind, label)
			if e.SizeBytes > 0 {
				line += fmt.Sprintf(" (%d bytes)", e.SizeBytes)
			}
			if e.LineCount > 0 {
				line += fmt.Sprintf(" — %d lines", e.LineCount)
			}
			b.WriteString(line)
			b.WriteString("\n")
		}
	}
	b.WriteString("\n")

	b.WriteString("## Correlation\n")
	if len(report.Assessment.Correlation) == 0 {
		b.WriteString("- (none)\n")
	} else {
		for _, c := range report.Assessment.Correlation {
			fmt.Fprintf(&b, "- %s\n", c)
		}
	}
	b.WriteString("\n")

	b.WriteString("## Likely Root Cause\n")
	fmt.Fprintf(&b, "**Confidence: %s**\n", report.Assessment.Confidence)
	b.WriteString(report.Assessment.LikelyRootCause)
	b.WriteString("\n\n")

	b.WriteString("## Unknowns / Gaps\n")
	if len(report.Assessment.Unknowns) == 0 {
		b.WriteString("- (none)\n")
	} else {
		for _, u := range report.Assessment.Unknowns {
			fmt.Fprintf(&b, "- %s\n", u)
		}
	}
	b.WriteString("\n")

	b.WriteString("## Suggested Next Steps\n")
	if len(report.Assessment.NextSteps) == 0 {
		b.WriteString("1. (none)\n")
	} else {
		for i, s := range report.Assessment.NextSteps {
			fmt.Fprintf(&b, "%d. %s\n", i+1, s)
		}
	}
	b.WriteString("\n")

	b.WriteString("## Suggested Internal Note\n")
	for _, line := range strings.Split(report.Assessment.SuggestedInternalNote, "\n") {
		fmt.Fprintf(&b, "> %s\n", line)
	}
	b.WriteString("\n")

	if opts.IncludeTimeline {
		b.WriteString("## Timeline\n")
		if len(report.Timeline) == 0 {
			b.WriteString("- (none)\n")
		} else {
			for _, ev := range report.Timeline {
				ts := "--unknown--"
				if ev.Timestamp != nil {
					ts = ev.Timestamp.UTC().Format("2006-01-02 15:04:05Z")
				}
				fmt.Fprintf(&b, "- %s  %s  %s\n", ts, ev.Source, ev.Message)
			}
		}
		b.WriteString("\n")
	}

	return b.String()
}
