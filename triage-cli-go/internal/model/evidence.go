package model

import "time"

// EvidenceKind enumerates the supported evidence sources.
type EvidenceKind string

const (
	EvidenceKindComment    EvidenceKind = "comment"
	EvidenceKindAttachment EvidenceKind = "attachment"
	EvidenceKindLocalFile  EvidenceKind = "local_file"
	EvidenceKindPaste      EvidenceKind = "paste"
)

// Evidence is a single piece of normalized evidence ingested into an
// investigation session. The Excerpt is a short preview suitable for
// inclusion in a rendered report.
type Evidence struct {
	Kind        EvidenceKind `json:"kind"`
	Source      string       `json:"source"`
	Label       string       `json:"label,omitempty"`
	SizeBytes   int64        `json:"size_bytes,omitempty"`
	ContentType string       `json:"content_type,omitempty"`
	Excerpt     string       `json:"excerpt,omitempty"`
	LineCount   int          `json:"line_count,omitempty"`
	CapturedAt  time.Time    `json:"captured_at"`
}
