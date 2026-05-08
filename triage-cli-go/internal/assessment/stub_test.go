package assessment

import (
	"context"
	"testing"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

func TestStubAssessor_Confidence(t *testing.T) {
	cases := []struct {
		name string
		ev   []model.Evidence
		want model.Confidence
	}{
		{
			name: "zero comments → unknown",
			ev:   nil,
			want: model.ConfidenceUnknown,
		},
		{
			name: "four comments → possible",
			ev: []model.Evidence{
				{Kind: model.EvidenceKindComment},
				{Kind: model.EvidenceKindComment},
				{Kind: model.EvidenceKindComment},
				{Kind: model.EvidenceKindComment},
			},
			want: model.ConfidencePossible,
		},
		{
			name: "five comments + local file → possible (stub never confirms)",
			ev: []model.Evidence{
				{Kind: model.EvidenceKindComment},
				{Kind: model.EvidenceKindComment},
				{Kind: model.EvidenceKindComment},
				{Kind: model.EvidenceKindComment},
				{Kind: model.EvidenceKindComment},
				{Kind: model.EvidenceKindLocalFile, LineCount: 250},
			},
			want: model.ConfidencePossible,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			a, err := StubAssessor{}.Assess(context.Background(), model.InvestigationSession{
				Ticket:   model.Ticket{ID: 1, Subject: "Subj.", Description: "Desc."},
				Evidence: tc.ev,
			})
			if err != nil {
				t.Fatalf("unexpected: %v", err)
			}
			if a.Confidence != tc.want {
				t.Fatalf("got %s, want %s", a.Confidence, tc.want)
			}
			if a.SuggestedInternalNote == "" {
				t.Fatal("internal note empty")
			}
		})
	}
}

// TestStubAssessor_HonestlySaysUnknown verifies that with zero/one
// evidence items the assessor refuses to claim a root cause.
func TestStubAssessor_HonestlySaysUnknown(t *testing.T) {
	a, err := StubAssessor{}.Assess(context.Background(), model.InvestigationSession{
		Ticket: model.Ticket{ID: 1, Subject: "Anything"},
	})
	if err != nil {
		t.Fatal(err)
	}
	if a.Confidence != model.ConfidenceUnknown {
		t.Fatalf("expected unknown, got %s", a.Confidence)
	}
}
