package zendesk

import (
	"context"
	"testing"
)

func TestMockFetcher_FallbackWhenNoFixturesDir(t *testing.T) {
	m := NewMockFetcher("")
	tkt, err := m.FetchTicket(context.Background(), 999)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if tkt.ID != 999 {
		t.Fatalf("got id %d, want 999", tkt.ID)
	}
	if tkt.Subject == "" {
		t.Fatal("expected non-empty subject")
	}
	if len(tkt.Comments) == 0 {
		t.Fatal("expected synthetic comments")
	}
}

func TestMockFetcher_FallbackWhenFixtureMissing(t *testing.T) {
	m := NewMockFetcher(t.TempDir())
	tkt, err := m.FetchTicket(context.Background(), 4242)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if tkt.ID != 4242 {
		t.Fatalf("got id %d, want 4242", tkt.ID)
	}
}

func TestMockFetcher_LoadsFixture(t *testing.T) {
	m := NewMockFetcher("../../testdata/tickets")
	tkt, err := m.FetchTicket(context.Background(), 12345)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if tkt.ID != 12345 {
		t.Fatalf("got id %d, want 12345", tkt.ID)
	}
}
