package tui

import (
	"fmt"
	"strings"

	"github.com/charmbracelet/bubbles/viewport"
	tea "github.com/charmbracelet/bubbletea"

	"github.com/midwestman35/triage-cli-go/internal/investigation"
	"github.com/midwestman35/triage-cli-go/internal/render"
)

// Update implements tea.Model.
func (m Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
		if !m.ready {
			m.timelineVP = viewport.New(msg.Width-4, 6)
			m.reportVP = viewport.New(msg.Width-4, 12)
			m.ready = true
		} else {
			m.timelineVP.Width = msg.Width - 4
			m.reportVP.Width = msg.Width - 4
		}
		return m, nil

	case tea.KeyMsg:
		switch msg.String() {
		case "ctrl+c", "q":
			if m.phase != phaseComplete {
				m.phase = phaseCancelled
			}
			return m, tea.Quit
		case "tab":
			m.focus = cycleFocus(m.focus, +1, m.phase == phaseComplete)
			return m, nil
		case "shift+tab":
			m.focus = cycleFocus(m.focus, -1, m.phase == phaseComplete)
			return m, nil
		case "enter":
			if m.phase == phaseComplete {
				m.focus = focusReport
				return m, nil
			}
		}
		// Forward arrow / pgup / pgdn to the focused viewport.
		var cmd tea.Cmd
		switch m.focus {
		case focusTimeline:
			m.timelineVP, cmd = m.timelineVP.Update(msg)
		case focusReport:
			m.reportVP, cmd = m.reportVP.Update(msg)
		}
		return m, cmd

	case PhaseEventMsg:
		return m.handlePhaseEvent(msg.Event), nil

	case TicketLoadedMsg:
		t := msg.Ticket
		m.ticket = &t
		// Seed the timeline pane with a header line so users see something.
		m.appendTimeline(fmt.Sprintf("loaded ZD-%d: %s", t.ID, t.Subject))
		return m, nil

	case EvidenceAddedMsg:
		m.appendTimeline(fmt.Sprintf("evidence: %s — %s", msg.Evidence.Kind, msg.Evidence.Label))
		return m, nil

	case AssessmentDoneMsg:
		m.appendTimeline(fmt.Sprintf("assessment: confidence=%s", msg.Assessment.Confidence))
		return m, nil

	case PipelineDoneMsg:
		m.report = &msg.Report
		m.finalMarkdown = render.Markdown(msg.Report, render.MarkdownOpts{IncludeTimeline: true})
		m.reportVP.SetContent(m.finalMarkdown)
		m.phase = phaseComplete
		// Mark every step done if we got here.
		for p := investigation.PhaseLoadTicket; p <= investigation.PhaseAssess; p++ {
			m.stepStatus[p] = stepDone
		}
		m.appendTimeline("pipeline complete · press [enter] to focus report viewer")
		return m, nil

	case PipelineErrorMsg:
		m.pipelineErr = msg.Err
		m.phase = phaseError
		if m.currentStep != 0 {
			m.stepStatus[m.currentStep] = stepFailed
		}
		m.appendTimeline("pipeline error: " + msg.Err.Error())
		return m, nil
	}
	return m, nil
}

func cycleFocus(f focus, dir int, complete bool) focus {
	options := []focus{focusActive, focusTimeline}
	if complete {
		options = append(options, focusReport)
	}
	idx := 0
	for i, o := range options {
		if o == f {
			idx = i
			break
		}
	}
	idx = (idx + dir + len(options)) % len(options)
	return options[idx]
}

func (m *Model) handlePhaseEvent(e investigation.Event) Model {
	if m.phase == phaseLoading {
		m.phase = phaseRunning
	}
	if e.Err != nil {
		m.stepStatus[e.Phase] = stepFailed
		m.appendTimeline(fmt.Sprintf("[%d/%d] %s failed: %v", e.Step, e.Total, e.Phase, e.Err))
		return *m
	}
	// Mark previous steps done, current active.
	for p := investigation.PhaseLoadTicket; p <= investigation.PhaseAssess; p++ {
		switch {
		case p < e.Phase:
			if m.stepStatus[p] == stepPending || m.stepStatus[p] == stepActive {
				m.stepStatus[p] = stepDone
			}
		case p == e.Phase:
			if e.Done {
				m.stepStatus[p] = stepDone
			} else {
				m.stepStatus[p] = stepActive
			}
		}
	}
	m.currentStep = e.Phase
	m.stepDetails[e.Phase] = e.Message
	m.appendTimeline(fmt.Sprintf("[%d/%d] %s", e.Step, e.Total, e.Message))
	return *m
}

func (m *Model) appendTimeline(line string) {
	m.timelineLines = append(m.timelineLines, line)
	content := strings.Join(m.timelineLines, "\n")
	m.timelineVP.SetContent(content)
	m.timelineVP.GotoBottom()
}
