package watcher

import (
	"path/filepath"
	"testing"
)

func TestLoadState_MissingFile(t *testing.T) {
	s, err := LoadState(filepath.Join(t.TempDir(), "missing.json"))
	if err != nil {
		t.Fatalf("unexpected: %v", err)
	}
	if s.Version != StateVersion {
		t.Fatalf("version: got %d", s.Version)
	}
	if s.Triaged == nil {
		t.Fatal("triaged map nil")
	}
}

func TestSaveLoadRoundTrip(t *testing.T) {
	path := filepath.Join(t.TempDir(), "watch.json")
	in := State{Version: StateVersion, Triaged: map[int64]string{42: "2026-05-08T14:30:00Z"}}
	if err := SaveState(path, in); err != nil {
		t.Fatalf("save: %v", err)
	}
	out, err := LoadState(path)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if out.Triaged[42] != "2026-05-08T14:30:00Z" {
		t.Fatalf("round trip lost data: %+v", out)
	}
}
