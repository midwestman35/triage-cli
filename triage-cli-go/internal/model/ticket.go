// Package model defines the core domain types used across the triage-cli
// pipeline. Types here are pure data carriers — no business logic, no I/O.
package model

import "time"

// Ticket is a normalized view of a Zendesk ticket relevant to triage.
type Ticket struct {
	ID             int64           `json:"id"`
	Subject        string          `json:"subject"`
	Description    string          `json:"description"`
	RequesterOrg   string          `json:"requester_org,omitempty"`
	Status         string          `json:"status,omitempty"`
	Priority       string          `json:"priority,omitempty"`
	CreatedAt      time.Time       `json:"created_at"`
	UpdatedAt      time.Time       `json:"updated_at"`
	Comments       []Comment       `json:"comments,omitempty"`
	AttachmentRefs []AttachmentRef `json:"attachments,omitempty"`
}

// Comment is a single Zendesk ticket comment, public or internal.
type Comment struct {
	ID         int64     `json:"id"`
	AuthorName string    `json:"author_name"`
	Public     bool      `json:"public"`
	Body       string    `json:"body"`
	CreatedAt  time.Time `json:"created_at"`
}

// AttachmentRef is metadata about a ticket attachment. The spike does not
// download attachment content — only the reference is surfaced.
type AttachmentRef struct {
	Filename    string `json:"filename"`
	ContentType string `json:"content_type"`
	SizeBytes   int64  `json:"size_bytes"`
	URL         string `json:"url,omitempty"`
}
