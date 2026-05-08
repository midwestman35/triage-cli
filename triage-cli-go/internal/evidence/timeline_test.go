package evidence

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

func TestBuildTimeline_OrderingAndUntimestampedLast(t *testing.T) {
	base := time.Date(2026, 5, 8, 13, 0, 0, 0, time.UTC)
	ticket := model.Ticket{
		ID:        100,
		Subject:   "Test ticket",
		CreatedAt: base,
		Comments: []model.Comment{
			{ID: 1, AuthorName: "B", Public: true, Body: "second", CreatedAt: base.Add(20 * time.Minute)},
			{ID: 2, AuthorName: "A", Public: false, Body: "first", CreatedAt: base.Add(10 * time.Minute)},
		},
	}
	ev := []model.Evidence{
		// untimestamped (zero time)
		{Kind: model.EvidenceKindLocalFile, Source: "file:/x", Label: "x.log"},
		// timestamped after the comments
		{Kind: model.EvidenceKindLocalFile, Source: "file:/y", Label: "y.log", CapturedAt: base.Add(40 * time.Minute)},
	}
	tl := BuildTimeline(ticket, ev)
	// 1 ticket_created + 2 comments + 2 non-comment evidence = 5
	if len(tl) != 5 {
		t.Fatalf("expected 5 events, got %d", len(tl))
	}
	if tl[0].Kind != "ticket_created" {
		t.Fatalf("first event should be ticket_created, got %q", tl[0].Kind)
	}
	if tl[1].Message == "" || tl[1].Timestamp == nil || !tl[1].Timestamp.Equal(base.Add(10*time.Minute)) {
		t.Fatalf("expected earliest comment second, got %+v", tl[1])
	}
	if tl[len(tl)-1].Timestamp != nil {
		t.Fatalf("expected untimestamped event last, got %+v", tl[len(tl)-1])
	}
}

func TestFromLocalFile(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "sample.log")
	body := "line one\nline two\nline three\n"
	if err := os.WriteFile(p, []byte(body), 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}
	stamp := time.Date(2026, 5, 8, 12, 0, 0, 0, time.UTC)
	ev, err := FromLocalFile(p, stamp)
	if err != nil {
		t.Fatalf("unexpected: %v", err)
	}
	if ev.Kind != model.EvidenceKindLocalFile {
		t.Fatalf("kind: %s", ev.Kind)
	}
	if ev.LineCount != 3 {
		t.Fatalf("line count = %d, want 3", ev.LineCount)
	}
	if ev.Excerpt == "" {
		t.Fatal("excerpt empty")
	}
	if !ev.CapturedAt.Equal(stamp) {
		t.Fatalf("captured_at = %v, want %v", ev.CapturedAt, stamp)
	}
}

func TestFromLocalFile_Missing(t *testing.T) {
	_, err := FromLocalFile(filepath.Join(t.TempDir(), "missing"), time.Now())
	if err == nil {
		t.Fatal("expected error for missing file")
	}
}
