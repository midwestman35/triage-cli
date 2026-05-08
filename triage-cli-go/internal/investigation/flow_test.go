package investigation

import (
	"context"
	"testing"
	"time"

	"github.com/midwestman35/triage-cli-go/internal/assessment"
	"github.com/midwestman35/triage-cli-go/internal/zendesk"
)

func TestRun_WithMockFetcherAndStubAssessor(t *testing.T) {
	deps := Deps{
		Fetcher:  zendesk.NewMockFetcher("../../testdata/tickets"),
		Assessor: assessment.StubAssessor{},
		Now:      func() time.Time { return time.Date(2026, 5, 8, 14, 30, 0, 0, time.UTC) },
	}
	report, err := Run(context.Background(), deps, RunOpts{TicketID: 12345})
	if err != nil {
		t.Fatalf("run: %v", err)
	}
	if report.Tool == "" || report.Assessment.Summary == "" {
		t.Fatal("empty report fields")
	}
	if len(report.Timeline) == 0 {
		t.Fatal("timeline empty")
	}
	foundComment := false
	for _, s := range report.Sources {
		if s == "comment" {
			foundComment = true
		}
	}
	if !foundComment {
		t.Fatalf("expected sources to include comment; got %v", report.Sources)
	}
}

func TestRun_ChanReporterEmitsAllPhasesInOrder(t *testing.T) {
	// Buffered to TotalPhases+1 (final Done event) so non-blocking
	// sends never drop in this test. The ChanReporter does drop
	// under back-pressure; we size accordingly.
	ch := make(chan Event, TotalPhases+2)
	deps := Deps{
		Fetcher:  zendesk.NewMockFetcher("../../testdata/tickets"),
		Assessor: assessment.StubAssessor{},
		Now:      func() time.Time { return time.Date(2026, 5, 8, 14, 30, 0, 0, time.UTC) },
		Reporter: ChanReporter{Ch: ch},
	}
	if _, err := Run(context.Background(), deps, RunOpts{TicketID: 12345}); err != nil {
		t.Fatalf("run: %v", err)
	}
	close(ch)

	want := []Phase{
		PhaseLoadTicket,
		PhaseReviewComments,
		PhaseCatalogueAttachments,
		PhaseIngestEvidence,
		PhaseBuildTimeline,
		PhaseAssess,
	}
	var got []Event
	for e := range ch {
		got = append(got, e)
	}
	if len(got) < len(want)+1 {
		t.Fatalf("expected at least %d events (6 phases + done), got %d", len(want)+1, len(got))
	}
	for i, phase := range want {
		if got[i].Phase != phase {
			t.Errorf("event[%d]: phase = %v, want %v", i, got[i].Phase, phase)
		}
		if got[i].Total != TotalPhases {
			t.Errorf("event[%d]: Total = %d, want %d", i, got[i].Total, TotalPhases)
		}
		if got[i].Step != int(phase) {
			t.Errorf("event[%d]: Step = %d, want %d", i, got[i].Step, int(phase))
		}
	}
	final := got[len(got)-1]
	if !final.Done {
		t.Errorf("final event Done = false, want true")
	}
}

func TestNopReporter_DoesNotPanicOnNilDeps(t *testing.T) {
	deps := Deps{
		Fetcher:  zendesk.NewMockFetcher("../../testdata/tickets"),
		Assessor: assessment.StubAssessor{},
		Now:      func() time.Time { return time.Date(2026, 5, 8, 14, 30, 0, 0, time.UTC) },
		// Reporter intentionally nil — Run must default to NopReporter.
	}
	if _, err := Run(context.Background(), deps, RunOpts{TicketID: 12345}); err != nil {
		t.Fatalf("run: %v", err)
	}
}
