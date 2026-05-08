package tui

import (
	"context"
	"errors"
	"fmt"

	tea "github.com/charmbracelet/bubbletea"

	"github.com/midwestman35/triage-cli-go/internal/investigation"
	"github.com/midwestman35/triage-cli-go/internal/model"
)

// ErrUserCancelled is returned by Run when the user pressed q/ctrl+c
// before the pipeline completed. Callers should NOT save artifacts in
// that case.
var ErrUserCancelled = errors.New("triage cancelled by user")

// Runner kicks off the underlying pipeline. The TUI calls it once, in
// a goroutine, with channels to stream events and the final result.
// The runner MUST close eventCh, ticketCh (after the ticket is sent or
// fetch fails), and one of doneCh/errCh exactly once.
type Runner func(
	eventCh chan<- investigation.Event,
	ticketCh chan<- model.Ticket,
	doneCh chan<- *model.TriageReport,
	errCh chan<- error,
)

// Run starts the bubbletea program and the runner, wires events, and
// returns the final report (or ErrUserCancelled, or a pipeline error).
func Run(ctx context.Context, ticketID int64, noColor bool, runner Runner) (*model.TriageReport, error) {
	eventCh := make(chan investigation.Event, 32)
	ticketCh := make(chan model.Ticket, 1)
	doneCh := make(chan *model.TriageReport, 1)
	errCh := make(chan error, 1)

	m := New(ticketID, noColor)
	prog := tea.NewProgram(m, tea.WithAltScreen(), tea.WithContext(ctx))

	// Pump runner events into the bubbletea program.
	pumpDone := make(chan struct{})
	go func() {
		defer close(pumpDone)
		for {
			select {
			case ev, ok := <-eventCh:
				if !ok {
					eventCh = nil
				} else {
					prog.Send(PhaseEventMsg{Event: ev})
				}
			case t, ok := <-ticketCh:
				if !ok {
					ticketCh = nil
				} else {
					prog.Send(TicketLoadedMsg{Ticket: t})
				}
			case rep := <-doneCh:
				if rep != nil {
					prog.Send(PipelineDoneMsg{Report: *rep})
				}
				return
			case e := <-errCh:
				if e != nil {
					prog.Send(PipelineErrorMsg{Err: e})
				}
				return
			}
			if eventCh == nil && ticketCh == nil {
				// All inbound channels closed without a done/err — wait for one.
				select {
				case rep := <-doneCh:
					if rep != nil {
						prog.Send(PipelineDoneMsg{Report: *rep})
					}
				case e := <-errCh:
					if e != nil {
						prog.Send(PipelineErrorMsg{Err: e})
					}
				}
				return
			}
		}
	}()

	// Run the pipeline in its own goroutine.
	go runner(eventCh, ticketCh, doneCh, errCh)

	finalModel, err := prog.Run()
	<-pumpDone
	if err != nil {
		return nil, fmt.Errorf("tui program: %w", err)
	}
	mm, ok := finalModel.(Model)
	if !ok {
		return nil, errors.New("tui: unexpected model type")
	}
	if mm.phase == phaseCancelled {
		return nil, ErrUserCancelled
	}
	if mm.phase == phaseError {
		if mm.pipelineErr != nil {
			return nil, mm.pipelineErr
		}
		return nil, errors.New("tui: pipeline ended in error state")
	}
	if mm.report == nil {
		return nil, ErrUserCancelled
	}
	return mm.report, nil
}
