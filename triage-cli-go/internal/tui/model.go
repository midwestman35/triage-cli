// Package tui implements a Bubble Tea three-pane TUI for the
// `investigate --tui` flow. It consumes the same investigation pipeline
// as the linear CLI but renders progress and the final report
// interactively. The package is opt-in behind --tui; the linear stderr
// flow remains the default for piping/CI.
package tui

import (
	"github.com/charmbracelet/bubbles/viewport"
	tea "github.com/charmbracelet/bubbletea"

	"github.com/midwestman35/triage-cli-go/internal/investigation"
	"github.com/midwestman35/triage-cli-go/internal/model"
)

// uiPhase is the high-level state of the TUI program.
type uiPhase int

const (
	phaseLoading uiPhase = iota
	phaseRunning
	phaseComplete
	phaseError
	phaseCancelled
)

// focus selects which pane receives scroll keys.
type focus int

const (
	focusActive focus = iota
	focusTimeline
	focusReport
)

// Model is the bubbletea.Model for the investigate TUI.
type Model struct {
	// configuration
	ticketID int64
	noColor  bool

	// pipeline state
	phase       uiPhase
	currentStep investigation.Phase
	stepStatus  map[investigation.Phase]stepState
	stepDetails map[investigation.Phase]string

	// data
	ticket        *model.Ticket
	timelineLines []string
	report        *model.TriageReport
	finalMarkdown string
	pipelineErr   error

	// viewport state
	timelineVP viewport.Model
	reportVP   viewport.Model
	focus      focus

	// dimensions
	width  int
	height int
	ready  bool
}

type stepState int

const (
	stepPending stepState = iota
	stepActive
	stepDone
	stepFailed
)

// New constructs a fresh TUI model for the given ticket.
func New(ticketID int64, noColor bool) Model {
	stepStatus := map[investigation.Phase]stepState{}
	stepDetails := map[investigation.Phase]string{}
	for p := investigation.PhaseLoadTicket; p <= investigation.PhaseAssess; p++ {
		stepStatus[p] = stepPending
	}
	return Model{
		ticketID:    ticketID,
		noColor:     noColor,
		phase:       phaseLoading,
		stepStatus:  stepStatus,
		stepDetails: stepDetails,
		focus:       focusActive,
	}
}

// Init implements tea.Model.
func (m Model) Init() tea.Cmd { return nil }
