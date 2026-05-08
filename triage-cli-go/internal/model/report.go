package model

import "time"

// TriageReport is the final structured output for one investigation,
// suitable for both Markdown rendering and JSON archival.
type TriageReport struct {
	TicketID    int64           `json:"ticket_id"`
	GeneratedAt time.Time       `json:"generated_at"`
	Sources     []string        `json:"sources"`
	Assessment  Assessment      `json:"assessment"`
	Evidence    []Evidence      `json:"evidence"`
	Timeline    []TimelineEvent `json:"timeline"`
	Tool        string          `json:"tool"`
}
