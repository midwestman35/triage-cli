package model

// Confidence indicates how strongly the assessment stands behind its
// stated likely root cause. The stub assessor never claims Confirmed
// or Likely — those tiers are reserved for future LLM-backed assessors.
type Confidence string

const (
	ConfidenceConfirmed Confidence = "confirmed"
	ConfidenceLikely    Confidence = "likely"
	ConfidencePossible  Confidence = "possible"
	ConfidenceUnknown   Confidence = "unknown"
)

// Assessment is the structured triage conclusion for a session.
type Assessment struct {
	Summary               string     `json:"summary"`
	LikelyRootCause       string     `json:"likely_root_cause"`
	Confidence            Confidence `json:"confidence"`
	Correlation           []string   `json:"correlation,omitempty"`
	Unknowns              []string   `json:"unknowns,omitempty"`
	NextSteps             []string   `json:"next_steps,omitempty"`
	SuggestedInternalNote string     `json:"suggested_internal_note"`
}
