package tui

import (
	"strings"
	"testing"
	"time"

	tea "github.com/charmbracelet/bubbletea"

	"github.com/midwestman35/triage-cli-go/internal/investigation"
	"github.com/midwestman35/triage-cli-go/internal/model"
)

// resize ships a WindowSizeMsg through Update so the model is "ready"
// and viewports are sized.
func resize(t *testing.T, m Model, w, h int) Model {
	t.Helper()
	out, _ := m.Update(tea.WindowSizeMsg{Width: w, Height: h})
	mm, ok := out.(Model)
	if !ok {
		t.Fatalf("unexpected model type: %T", out)
	}
	return mm
}

func TestView_RendersAt80x24WithKeyElements(t *testing.T) {
	m := New(12345, true)
	m = resize(t, m, 80, 24)

	v := m.View()
	if v == "" {
		t.Fatal("View() returned empty string")
	}

	wantSubstrings := []string{
		"ZD-12345",
		"Workflow",
		"Load ticket",
		"Review comments",
		"Catalogue attachments",
		"Ingest evidence",
		"Build timeline",
		"Assess",
		"Evidence / Timeline",
		"[q] quit",
	}
	for _, sub := range wantSubstrings {
		if !strings.Contains(v, sub) {
			t.Errorf("View output missing %q\n--- view ---\n%s\n--- end ---", sub, v)
		}
	}
}

func TestView_PhaseEventUpdatesWorkflowRail(t *testing.T) {
	m := New(12345, true)
	m = resize(t, m, 100, 30)

	// Mark first two phases done, third active.
	for _, e := range []investigation.Event{
		{Phase: investigation.PhaseLoadTicket, Step: 1, Total: 6, Message: "Loading…"},
		{Phase: investigation.PhaseReviewComments, Step: 2, Total: 6, Message: "Reviewing…"},
		{Phase: investigation.PhaseCatalogueAttachments, Step: 3, Total: 6, Message: "Cataloguing…"},
	} {
		out, _ := m.Update(PhaseEventMsg{Event: e})
		m = out.(Model)
	}

	if m.stepStatus[investigation.PhaseLoadTicket] != stepDone {
		t.Errorf("PhaseLoadTicket: want stepDone, got %v", m.stepStatus[investigation.PhaseLoadTicket])
	}
	if m.stepStatus[investigation.PhaseReviewComments] != stepDone {
		t.Errorf("PhaseReviewComments: want stepDone, got %v", m.stepStatus[investigation.PhaseReviewComments])
	}
	if m.stepStatus[investigation.PhaseCatalogueAttachments] != stepActive {
		t.Errorf("PhaseCatalogueAttachments: want stepActive, got %v", m.stepStatus[investigation.PhaseCatalogueAttachments])
	}
	if m.stepStatus[investigation.PhaseAssess] != stepPending {
		t.Errorf("PhaseAssess: want stepPending, got %v", m.stepStatus[investigation.PhaseAssess])
	}

	v := m.View()
	if !strings.Contains(v, "Cataloguing") {
		t.Errorf("View should reflect current phase message; got:\n%s", v)
	}
}

func TestUpdate_QuitOnQReturnsTeaQuit(t *testing.T) {
	m := New(12345, true)
	m = resize(t, m, 80, 24)

	out, cmd := m.Update(tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'q'}})
	mm := out.(Model)
	if mm.phase != phaseCancelled {
		t.Errorf("phase = %v, want phaseCancelled", mm.phase)
	}
	if cmd == nil {
		t.Fatal("expected tea.Quit command, got nil")
	}
	// Execute the cmd and check it returns a QuitMsg.
	msg := cmd()
	if _, ok := msg.(tea.QuitMsg); !ok {
		t.Errorf("expected tea.QuitMsg, got %T", msg)
	}
}

func TestUpdate_WindowResizeUpdatesDimensions(t *testing.T) {
	m := New(12345, true)
	out, _ := m.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	mm := out.(Model)
	if mm.width != 120 || mm.height != 40 {
		t.Errorf("dims = %dx%d, want 120x40", mm.width, mm.height)
	}
	if !mm.ready {
		t.Error("model should be ready after first WindowSizeMsg")
	}

	out2, _ := mm.Update(tea.WindowSizeMsg{Width: 80, Height: 24})
	mm2 := out2.(Model)
	if mm2.width != 80 || mm2.height != 24 {
		t.Errorf("dims after resize = %dx%d, want 80x24", mm2.width, mm2.height)
	}
}

func TestUpdate_PipelineDoneSwapsToReportView(t *testing.T) {
	m := New(12345, true)
	m = resize(t, m, 100, 30)

	report := model.TriageReport{
		TicketID:    12345,
		GeneratedAt: time.Date(2026, 5, 8, 14, 0, 0, 0, time.UTC),
		Tool:        "triage-cli test",
		Assessment: model.Assessment{
			Summary:               "Test summary",
			Confidence:            "likely",
			LikelyRootCause:       "Test cause",
			SuggestedInternalNote: "Test note",
		},
	}
	out, _ := m.Update(PipelineDoneMsg{Report: report})
	mm := out.(Model)
	if mm.phase != phaseComplete {
		t.Errorf("phase = %v, want phaseComplete", mm.phase)
	}
	if mm.report == nil {
		t.Fatal("report not stored")
	}
	if mm.finalMarkdown == "" {
		t.Error("finalMarkdown should be populated")
	}
	v := mm.View()
	if !strings.Contains(v, "Triage Report") {
		t.Errorf("complete view should show 'Triage Report' heading; got:\n%s", v)
	}
}

func TestView_SnapshotAt80x24(t *testing.T) {
	// Render snapshot for documentation / sanity. Asserts shape, not pixels.
	m := New(12345, true)
	m = resize(t, m, 80, 24)
	out, _ := m.Update(PhaseEventMsg{Event: investigation.Event{
		Phase: investigation.PhaseReviewComments, Step: 2, Total: 6,
		Message: "Reviewing comments (3 found)...",
	}})
	m = out.(Model)
	out2, _ := m.Update(TicketLoadedMsg{Ticket: model.Ticket{
		ID: 12345, Subject: "SBC jitter on PSAP-01", RequesterOrg: "Acme Co",
	}})
	m = out2.(Model)
	v := m.View()
	t.Logf("\n%s", v)
	if !strings.Contains(v, "SBC jitter") {
		t.Errorf("expected ticket subject in header; got:\n%s", v)
	}
}
