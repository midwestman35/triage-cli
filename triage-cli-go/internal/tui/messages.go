package tui

import (
	"github.com/midwestman35/triage-cli-go/internal/investigation"
	"github.com/midwestman35/triage-cli-go/internal/model"
)

// PhaseEventMsg wraps an investigation.Event for the TUI's Update loop.
type PhaseEventMsg struct{ Event investigation.Event }

// TicketLoadedMsg fires once the ticket is fetched, so the header can
// render subject + requester org as soon as it's known.
type TicketLoadedMsg struct{ Ticket model.Ticket }

// EvidenceAddedMsg appends a single evidence item to the bottom pane.
type EvidenceAddedMsg struct{ Evidence model.Evidence }

// AssessmentDoneMsg fires once the LLM/stub returns its assessment.
type AssessmentDoneMsg struct{ Assessment model.Assessment }

// PipelineDoneMsg signals end-of-run with the final report. The TUI
// transitions to phaseComplete and renders the report Markdown into
// its viewport.
type PipelineDoneMsg struct{ Report model.TriageReport }

// PipelineErrorMsg signals a fatal pipeline error.
type PipelineErrorMsg struct{ Err error }
