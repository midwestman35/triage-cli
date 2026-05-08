// Package investigation orchestrates the linear triage pipeline.
// It is the single owner of the fetch → ingest → assess → report
// sequence shared by both the `investigate` and `triage` commands.
package investigation

import (
	"context"
	"fmt"
	"os"
	"time"

	"github.com/midwestman35/triage-cli-go/internal/assessment"
	"github.com/midwestman35/triage-cli-go/internal/evidence"
	"github.com/midwestman35/triage-cli-go/internal/model"
	"github.com/midwestman35/triage-cli-go/internal/zendesk"
)

// Tool is the user-visible identifier embedded in every report.
const Tool = "triage-cli 0.1.0-spike"

// Deps wires in the runtime collaborators. The Now hook is injectable
// so tests can pin generated timestamps.
type Deps struct {
	Fetcher  zendesk.Fetcher
	Assessor assessment.Assessor
	Now      func() time.Time
}

// RunOpts controls a single Run invocation.
type RunOpts struct {
	TicketID       int64
	EvidencePaths  []string
	PastedEvidence []string
	Guided         bool
	Quiet          bool
}

// Run executes the linear triage pipeline and returns a fully populated
// TriageReport. I/O is confined to the injected fetcher and the local
// filesystem (for evidence paths). Errors fetching the ticket fail the
// run; errors loading individual evidence files are warned to stderr
// and skipped — operator-friendly rather than fail-fast.
func Run(ctx context.Context, deps Deps, opts RunOpts) (model.TriageReport, error) {
	if deps.Now == nil {
		deps.Now = func() time.Time { return time.Now().UTC() }
	}

	step := func(n, total int, msg string) {
		if opts.Guided && !opts.Quiet {
			fmt.Fprintf(os.Stderr, "→ [%d/%d] %s\n", n, total, msg)
		}
	}

	step(1, 6, fmt.Sprintf("Loading ticket ZD-%d...", opts.TicketID))
	ticket, err := deps.Fetcher.FetchTicket(ctx, opts.TicketID)
	if err != nil {
		return model.TriageReport{}, fmt.Errorf("fetch ticket %d: %w", opts.TicketID, err)
	}

	step(2, 6, fmt.Sprintf("Reviewing comments (%d found)...", len(ticket.Comments)))
	commentEv := evidence.FromComments(ticket.Comments)

	step(3, 6, fmt.Sprintf("Cataloguing attachments (%d found)...", len(ticket.AttachmentRefs)))
	attachEv := evidence.FromAttachmentRefs(ticket.AttachmentRefs)

	allEvidence := make([]model.Evidence, 0, len(commentEv)+len(attachEv)+len(opts.EvidencePaths)+len(opts.PastedEvidence))
	allEvidence = append(allEvidence, commentEv...)
	allEvidence = append(allEvidence, attachEv...)

	if len(opts.EvidencePaths) > 0 {
		step(4, 6, fmt.Sprintf("Ingesting %d local evidence file(s)...", len(opts.EvidencePaths)))
		for _, p := range opts.EvidencePaths {
			ev, err := evidence.FromLocalFile(p, deps.Now())
			if err != nil {
				fmt.Fprintf(os.Stderr, "! skipping evidence %q: %v\n", p, err)
				continue
			}
			allEvidence = append(allEvidence, ev)
		}
	} else {
		step(4, 6, "No local evidence supplied; skipping.")
	}

	for _, paste := range opts.PastedEvidence {
		allEvidence = append(allEvidence, evidence.FromPaste("", paste))
	}

	step(5, 6, "Building timeline...")
	timeline := evidence.BuildTimeline(ticket, allEvidence)

	step(6, 6, "Running assessment...")
	session := model.InvestigationSession{
		Ticket:   ticket,
		Evidence: allEvidence,
		Timeline: timeline,
	}
	a, err := deps.Assessor.Assess(ctx, session)
	if err != nil {
		return model.TriageReport{}, fmt.Errorf("assessment failed: %w", err)
	}

	report := model.TriageReport{
		TicketID:    ticket.ID,
		GeneratedAt: deps.Now(),
		Sources:     uniqueSources(allEvidence),
		Assessment:  a,
		Evidence:    allEvidence,
		Timeline:    timeline,
		Tool:        Tool,
	}
	return report, nil
}

func uniqueSources(ev []model.Evidence) []string {
	seen := map[model.EvidenceKind]bool{}
	out := []string{}
	for _, e := range ev {
		if seen[e.Kind] {
			continue
		}
		seen[e.Kind] = true
		out = append(out, string(e.Kind))
	}
	return out
}
