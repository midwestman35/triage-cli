package evidence

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// FromLocalFile reads a file from disk and returns it as evidence.
// The full file is loaded for line counting; only the first ~500 chars
// are kept as the excerpt for the report. The capturedAt argument
// stamps the resulting evidence so callers can inject a fixed clock
// for deterministic output (tests, watcher batches).
func FromLocalFile(path string, capturedAt time.Time) (model.Evidence, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return model.Evidence{}, fmt.Errorf("resolve path %q: %w", path, err)
	}
	info, err := os.Stat(abs)
	if err != nil {
		return model.Evidence{}, fmt.Errorf("stat %s: %w", abs, err)
	}
	if info.IsDir() {
		return model.Evidence{}, fmt.Errorf("%s is a directory, expected a file", abs)
	}
	raw, err := os.ReadFile(abs)
	if err != nil {
		return model.Evidence{}, fmt.Errorf("read %s: %w", abs, err)
	}
	text := string(raw)
	lines := strings.Count(text, "\n")
	if len(text) > 0 && !strings.HasSuffix(text, "\n") {
		lines++
	}
	return model.Evidence{
		Kind:       model.EvidenceKindLocalFile,
		Source:     "file:" + abs,
		Label:      filepath.Base(abs),
		SizeBytes:  info.Size(),
		Excerpt:    excerpt(text, excerptLimit),
		LineCount:  lines,
		CapturedAt: capturedAt.UTC(),
	}, nil
}
