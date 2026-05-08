package tui

import "github.com/charmbracelet/lipgloss"

// palette holds the styles used across the TUI. When noColor is set we
// strip foreground/background colors but keep layout styles so panes
// still render with borders and padding.
type palette struct {
	border   lipgloss.Style
	header   lipgloss.Style
	heading  lipgloss.Style
	dim      lipgloss.Style
	success  lipgloss.Style
	info     lipgloss.Style
	warning  lipgloss.Style
	errorSty lipgloss.Style
	footer   lipgloss.Style
}

func newPalette(noColor bool) palette {
	if noColor {
		base := lipgloss.NewStyle()
		return palette{
			border:   lipgloss.NewStyle().Border(lipgloss.RoundedBorder()),
			header:   base.Bold(true),
			heading:  base.Bold(true),
			dim:      base.Faint(true),
			success:  base,
			info:     base,
			warning:  base,
			errorSty: base.Bold(true),
			footer:   base.Faint(true),
		}
	}
	return palette{
		border:   lipgloss.NewStyle().Border(lipgloss.RoundedBorder()).BorderForeground(lipgloss.Color("63")),
		header:   lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("87")),
		heading:  lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("219")),
		dim:      lipgloss.NewStyle().Foreground(lipgloss.Color("244")),
		success:  lipgloss.NewStyle().Foreground(lipgloss.Color("78")),
		info:     lipgloss.NewStyle().Foreground(lipgloss.Color("75")),
		warning:  lipgloss.NewStyle().Foreground(lipgloss.Color("215")),
		errorSty: lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("203")),
		footer:   lipgloss.NewStyle().Foreground(lipgloss.Color("244")),
	}
}
