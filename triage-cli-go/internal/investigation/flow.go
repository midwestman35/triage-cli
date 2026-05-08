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

// TotalPhases is the number of phases emitted by Run. Reporters that
// render progress bars use this as the denominator.
const TotalPhases = 6

// Phase identifies a stage in the pipeline.
type Phase int

const (
	PhaseLoadTicket Phase = iota + 1
	PhaseReviewComments
	PhaseCatalogueAttachments
	PhaseIngestEvidence
	PhaseBuildTimeline
	PhaseAssess
)

// String returns a short human label for a phase.
func (p Phase) String() string {
	switch p {
	case PhaseLoadTicket:
		return "Load ticket"
	case PhaseReviewComments:
		return "Review comments"
	case PhaseCatalogueAttachments:
		return "Catalogue attachments"
	case PhaseIngestEvidence:
		return "Ingest evidence"
	case PhaseBuildTimeline:
		return "Build timeline"
	case PhaseAssess:
		return "Assess"
	default:
		return fmt.Sprintf("phase-%d", int(p))
	}
}

// Event is a single phase boundary emitted from Run.
type Event struct {
	Phase   Phase
	Total   int    // total phases (always TotalPhases for now)
	Step    int    // numeric step counter (matches Phase)
	Message string // human-readable detail for this phase
	Err     error  // non-nil for failure events; pipeline aborts after
	Done    bool   // true on the final event (success only)
}

// Reporter consumes phase events. Implementations must be safe to call
// from the goroutine that runs the pipeline.
type Reporter interface {
	Report(Event)
}

// NopReporter discards all events. Used by triage (non-guided) mode and
// tests that want a quiet run.
type NopReporter struct{}

// Report implements Reporter.
func (NopReporter) Report(Event) {}

// StderrReporter writes step-numbered lines to stderr. When Quiet is
// true it discards events (matching the pre-Reporter behavior).
type StderrReporter struct{ Quiet bool }

// Report implements Reporter.
func (r StderrReporter) Report(e Event) {
	if r.Quiet {
		return
	}
	if e.Err != nil {
		fmt.Fprintf(os.Stderr, "! [%d/%d] %s failed: %v\n", e.Step, e.Total, e.Phase, e.Err)
		return
	}
	if e.Done {
		// The terminal report itself follows on stdout; the Done event
		// is for TUI consumers and would be a redundant trailing line
		// on stderr.
		return
	}
	fmt.Fprintf(os.Stderr, "→ [%d/%d] %s\n", e.Step, e.Total, e.Message)
}

// ChanReporter forwards events onto a channel for consumers like the
// Bubble Tea TUI. Sends are non-blocking — if the consumer is slow,
// events are dropped rather than stalling the pipeline. The TUI is
// expected to drain promptly, so dropped events should be rare.
type ChanReporter struct{ Ch chan<- Event }

// Report implements Reporter.
func (r ChanReporter) Report(e Event) {
	if r.Ch == nil {
		return
	}
	select {
	case r.Ch <- e:
	default:
	}
}

// Deps wires in the runtime collaborators. The Now hook is injectable
// so tests can pin generated timestamps. Reporter is optional; nil is
// treated as NopReporter.
type Deps struct {
	Fetcher  zendesk.Fetcher
	Assessor assessment.Assessor
	Now      func() time.Time
	Reporter Reporter
}

// RunOpts controls a single Run invocation.
type RunOpts struct {
	TicketID       int64
	EvidencePaths  []string
	PastedEvidence []string
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
	reporter := deps.Reporter
	if reporter == nil {
		reporter = NopReporter{}
	}

	emit := func(phase Phase, msg string) {
		reporter.Report(Event{
			Phase:   phase,
			Total:   TotalPhases,
			Step:    int(phase),
			Message: msg,
		})
	}
	emitErr := func(phase Phase, err error) {
		reporter.Report(Event{
			Phase: phase,
			Total: TotalPhases,
			Step:  int(phase),
			Err:   err,
		})
	}

	emit(PhaseLoadTicket, fmt.Sprintf("Loading ticket ZD-%d...", opts.TicketID))
	ticket, err := deps.Fetcher.FetchTicket(ctx, opts.TicketID)
	if err != nil {
		wrapped := fmt.Errorf("fetch ticket %d: %w", opts.TicketID, err)
		emitErr(PhaseLoadTicket, wrapped)
		return model.TriageReport{}, wrapped
	}

	emit(PhaseReviewComments, fmt.Sprintf("Reviewing comments (%d found)...", len(ticket.Comments)))
	commentEv := evidence.FromComments(ticket.Comments)

	emit(PhaseCatalogueAttachments, fmt.Sprintf("Cataloguing attachments (%d found)...", len(ticket.AttachmentRefs)))
	attachEv := evidence.FromAttachmentRefs(ticket.AttachmentRefs)

	allEvidence := make([]model.Evidence, 0, len(commentEv)+len(attachEv)+len(opts.EvidencePaths)+len(opts.PastedEvidence))
	allEvidence = append(allEvidence, commentEv...)
	allEvidence = append(allEvidence, attachEv...)

	if len(opts.EvidencePaths) > 0 {
		emit(PhaseIngestEvidence, fmt.Sprintf("Ingesting %d local evidence file(s)...", len(opts.EvidencePaths)))
		for _, p := range opts.EvidencePaths {
			ev, err := evidence.FromLocalFile(p, deps.Now())
			if err != nil {
				fmt.Fprintf(os.Stderr, "! skipping evidence %q: %v\n", p, err)
				continue
			}
			allEvidence = append(allEvidence, ev)
		}
	} else {
		emit(PhaseIngestEvidence, "No local evidence supplied; skipping.")
	}

	for _, paste := range opts.PastedEvidence {
		allEvidence = append(allEvidence, evidence.FromPaste("", paste))
	}

	emit(PhaseBuildTimeline, "Building timeline...")
	timeline := evidence.BuildTimeline(ticket, allEvidence)

	emit(PhaseAssess, "Running assessment...")
	session := model.InvestigationSession{
		Ticket:   ticket,
		Evidence: allEvidence,
		Timeline: timeline,
	}
	a, err := deps.Assessor.Assess(ctx, session)
	if err != nil {
		wrapped := fmt.Errorf("assessment failed: %w", err)
		emitErr(PhaseAssess, wrapped)
		return model.TriageReport{}, wrapped
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
	reporter.Report(Event{
		Phase:   PhaseAssess,
		Total:   TotalPhases,
		Step:    TotalPhases,
		Message: "Assessment complete.",
		Done:    true,
	})
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
