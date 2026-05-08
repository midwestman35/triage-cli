package model

import "time"

// TimelineEvent is a single normalized event for the investigation timeline.
// Timestamp is nil for untimestamped evidence.
type TimelineEvent struct {
	Timestamp *time.Time `json:"timestamp,omitempty"`
	Source    string     `json:"source"`
	Kind      string     `json:"kind"`
	Message   string     `json:"message"`
	RawRef    string     `json:"raw_ref,omitempty"`
}
