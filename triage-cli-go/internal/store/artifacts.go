// Package store writes paired Markdown / JSON triage artifacts to disk.
package store

import (
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// Artifact reports the absolute paths of the saved files.
type Artifact struct {
	MarkdownPath string
	JSONPath     string
}

// SaveArtifacts writes md and jsonBytes into outputDir as a paired
// `<id>-<UTC timestamp>.md` and `.json`. Files are written atomically
// via a tempfile + rename, and outputDir is created if missing.
func SaveArtifacts(outputDir string, ticketID int64, generatedAt time.Time, md string, jsonBytes []byte) (Artifact, error) {
	if err := os.MkdirAll(outputDir, 0o755); err != nil {
		return Artifact{}, fmt.Errorf("create output dir: %w", err)
	}
	stamp := generatedAt.UTC().Format("20060102-150405")
	base := fmt.Sprintf("%d-%s", ticketID, stamp)
	mdPath := filepath.Join(outputDir, base+".md")
	jsonPath := filepath.Join(outputDir, base+".json")

	if err := atomicWrite(mdPath, []byte(md)); err != nil {
		return Artifact{}, err
	}
	if err := atomicWrite(jsonPath, jsonBytes); err != nil {
		return Artifact{}, err
	}

	mdAbs, err := filepath.Abs(mdPath)
	if err != nil {
		return Artifact{}, err
	}
	jsonAbs, err := filepath.Abs(jsonPath)
	if err != nil {
		return Artifact{}, err
	}
	return Artifact{MarkdownPath: mdAbs, JSONPath: jsonAbs}, nil
}

// atomicWrite writes data to dst by first writing to dst+".tmp" and
// then renaming, so concurrent readers never see a partial file. The
// tempfile is removed on any failure so it never lingers on disk.
func atomicWrite(dst string, data []byte) (err error) {
	tmp := dst + ".tmp"
	defer func() {
		if err != nil {
			_ = os.Remove(tmp)
		}
	}()
	if err = os.WriteFile(tmp, data, 0o644); err != nil {
		return fmt.Errorf("write %s: %w", tmp, err)
	}
	if err = os.Rename(tmp, dst); err != nil {
		return fmt.Errorf("rename %s -> %s: %w", tmp, dst, err)
	}
	return nil
}
