package store

import (
	"os"
	"strings"
	"testing"
	"time"
)

func TestSaveArtifacts(t *testing.T) {
	dir := t.TempDir()
	when := time.Date(2026, 5, 8, 14, 30, 0, 0, time.UTC)
	art, err := SaveArtifacts(dir, 12345, when, "# md\n", []byte("{\"k\":1}\n"))
	if err != nil {
		t.Fatalf("save: %v", err)
	}
	mdRaw, err := os.ReadFile(art.MarkdownPath)
	if err != nil {
		t.Fatalf("read md: %v", err)
	}
	if !strings.Contains(string(mdRaw), "# md") {
		t.Fatalf("md body unexpected: %q", string(mdRaw))
	}
	jsonRaw, err := os.ReadFile(art.JSONPath)
	if err != nil {
		t.Fatalf("read json: %v", err)
	}
	if string(jsonRaw) != "{\"k\":1}\n" {
		t.Fatalf("json body unexpected: %q", string(jsonRaw))
	}
	if !strings.Contains(art.MarkdownPath, "12345-20260508-143000.md") {
		t.Fatalf("unexpected filename: %s", art.MarkdownPath)
	}
}
