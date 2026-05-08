package evidence

import (
	"strings"
	"time"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// FromPaste wraps a block of pasted text as evidence. The label is
// surfaced in the report; if empty, "pasted text" is used.
func FromPaste(label, text string) model.Evidence {
	if label == "" {
		label = "pasted text"
	}
	lines := strings.Count(text, "\n")
	if len(text) > 0 && !strings.HasSuffix(text, "\n") {
		lines++
	}
	return model.Evidence{
		Kind:       model.EvidenceKindPaste,
		Source:     "paste:" + label,
		Label:      label,
		SizeBytes:  int64(len(text)),
		Excerpt:    excerpt(text, excerptLimit),
		LineCount:  lines,
		CapturedAt: time.Now().UTC(),
	}
}
