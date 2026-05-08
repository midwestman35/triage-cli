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
	report, err := Run(context.Background(), deps, RunOpts{
		TicketID: 12345,
		Guided:   false,
		Quiet:    true,
	})
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
