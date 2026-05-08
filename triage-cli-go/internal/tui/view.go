package tui

import (
	"fmt"
	"strings"

	"github.com/charmbracelet/lipgloss"

	"github.com/midwestman35/triage-cli-go/internal/investigation"
)

// View implements tea.Model.
func (m Model) View() string {
	if !m.ready || m.width < 40 || m.height < 10 {
		return "triage-cli TUI — initialising (resize terminal if this persists)…"
	}

	pal := newPalette(m.noColor)
	header := m.renderHeader(pal)
	footer := m.renderFooter(pal)

	// Reserve lines: header (1) + blank (0) + footer (1) + 4 border lines
	// (top of row 1, divider between row 1 and row 2, bottom). Use a
	// simple budget: header + footer + 2 rows of bordered panes.
	bodyHeight := m.height - lipgloss.Height(header) - lipgloss.Height(footer)
	if bodyHeight < 6 {
		bodyHeight = 6
	}

	topHeight := bodyHeight / 2
	bottomHeight := bodyHeight - topHeight
	if topHeight < 5 {
		topHeight = 5
	}
	if bottomHeight < 4 {
		bottomHeight = 4
	}

	leftWidth := m.width / 3
	if leftWidth < 24 {
		leftWidth = 24
	}
	rightWidth := m.width - leftWidth - 4 // border chars
	if rightWidth < 20 {
		rightWidth = 20
	}

	workflow := m.renderWorkflow(pal, leftWidth-2, topHeight-2)
	active := m.renderActive(pal, rightWidth-2, topHeight-2)
	bottom := m.renderBottom(pal, m.width-4, bottomHeight-2)

	leftBox := pal.border.Width(leftWidth).Height(topHeight).Render(workflow)
	rightBox := pal.border.Width(rightWidth).Height(topHeight).Render(active)
	topRow := lipgloss.JoinHorizontal(lipgloss.Top, leftBox, rightBox)

	bottomBox := pal.border.Width(m.width - 2).Height(bottomHeight).Render(bottom)

	return strings.Join([]string{header, topRow, bottomBox, footer}, "\n")
}

func (m Model) renderHeader(pal palette) string {
	status := "running"
	switch m.phase {
	case phaseComplete:
		status = "complete"
	case phaseError:
		status = "error"
	case phaseCancelled:
		status = "cancelled"
	case phaseLoading:
		status = "loading"
	}
	subject := ""
	if m.ticket != nil {
		subject = " · " + truncate(m.ticket.Subject, 40)
	}
	sources := "sources: zendesk"
	if m.report != nil && len(m.report.Sources) > 0 {
		sources = "sources: " + strings.Join(m.report.Sources, ", ")
	}
	title := fmt.Sprintf("triage-cli · ZD-%d%s · %s", m.ticketID, subject, status)
	return pal.header.Render(title) + "\n" + pal.dim.Render(sources)
}

func (m Model) renderFooter(pal palette) string {
	if m.phase == phaseComplete {
		return pal.footer.Render("[q] quit · [tab] focus · [↑↓ pgup/pgdn] scroll · [enter] focus report")
	}
	if m.phase == phaseError {
		return pal.footer.Render("[q] quit")
	}
	return pal.footer.Render("[q] quit · running pipeline…")
}

func (m Model) renderWorkflow(pal palette, w, h int) string {
	_ = w
	_ = h
	var b strings.Builder
	b.WriteString(pal.heading.Render("Workflow"))
	b.WriteString("\n\n")
	for p := investigation.PhaseLoadTicket; p <= investigation.PhaseAssess; p++ {
		marker := "  "
		label := p.String()
		switch m.stepStatus[p] {
		case stepDone:
			marker = pal.success.Render("✓ ")
		case stepActive:
			marker = pal.info.Render("→ ")
			label = pal.info.Render(label)
		case stepFailed:
			marker = pal.errorSty.Render("✗ ")
		default:
			marker = pal.dim.Render("• ")
			label = pal.dim.Render(label)
		}
		b.WriteString(marker)
		b.WriteString(label)
		b.WriteString("\n")
	}
	// Trailing "Export" pseudo-phase mirrors the spec layout.
	exportMark := pal.dim.Render("• ")
	exportLabel := pal.dim.Render("Export")
	if m.phase == phaseComplete {
		exportMark = pal.success.Render("✓ ")
		exportLabel = "Export"
	}
	b.WriteString(exportMark)
	b.WriteString(exportLabel)
	return b.String()
}

func (m Model) renderActive(pal palette, w, h int) string {
	_ = w
	_ = h
	if m.phase == phaseComplete && m.report != nil {
		// Replace active pane with the report viewer.
		m.reportVP.Width = w
		m.reportVP.Height = h - 2
		return pal.heading.Render("Triage Report") + "\n\n" + m.reportVP.View()
	}
	if m.phase == phaseError && m.pipelineErr != nil {
		return pal.errorSty.Render("Pipeline error") + "\n\n" + m.pipelineErr.Error()
	}
	heading := "Active Step"
	detail := "Waiting for first phase event…"
	if m.currentStep != 0 {
		heading = m.currentStep.String()
		if d, ok := m.stepDetails[m.currentStep]; ok && d != "" {
			detail = d
		}
	}
	body := pal.heading.Render(heading) + "\n\n" + detail
	if m.ticket != nil {
		body += "\n\n" + pal.dim.Render(fmt.Sprintf(
			"Ticket: %s\nRequester: %s\nComments: %d · Attachments: %d",
			truncate(m.ticket.Subject, w-12),
			m.ticket.RequesterOrg,
			len(m.ticket.Comments),
			len(m.ticket.AttachmentRefs),
		))
	}
	return body
}

func (m Model) renderBottom(pal palette, w, h int) string {
	_ = w
	heading := pal.heading.Render("Evidence / Timeline")
	if m.phase == phaseComplete {
		heading = pal.heading.Render("Evidence / Timeline (complete)")
	}
	m.timelineVP.Width = w
	m.timelineVP.Height = h - 2
	if len(m.timelineLines) == 0 {
		return heading + "\n\n" + pal.dim.Render("(no events yet)")
	}
	return heading + "\n\n" + m.timelineVP.View()
}

func truncate(s string, n int) string {
	if n <= 0 {
		return ""
	}
	if len(s) <= n {
		return s
	}
	if n <= 1 {
		return s[:n]
	}
	return s[:n-1] + "…"
}
