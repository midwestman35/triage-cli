// Package cli wires the cobra command tree for triage-cli.
package cli

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"
)

// Version is the user-visible version string for the spike.
const Version = "0.1.0-spike"

// globalFlags holds flags exposed on the root command. They are read
// by subcommands directly from the parsed root command.
type globalFlags struct {
	noColor    bool
	outputDir  string
	quiet      bool
	configPath string
}

var globals = &globalFlags{}

// newRoot builds the root cobra command and wires every subcommand.
func newRoot() *cobra.Command {
	root := &cobra.Command{
		Use:   "triage-cli",
		Short: "Guided Zendesk ticket investigation assistant",
		Long: `triage-cli is a guided Zendesk ticket investigation assistant.

It loads a ticket, reviews comments and attachments, ingests local evidence,
builds a timeline, and produces a structured triage report — paired Markdown
and JSON artifacts, with a deterministic stub assessment in the spike.

Stdout is reserved for the rendered report so output is pipe-friendly;
status, warnings, and progress go to stderr.`,
		SilenceUsage: true,
	}

	root.PersistentFlags().BoolVar(&globals.noColor, "no-color", false, "disable ANSI color in output")
	root.PersistentFlags().StringVar(&globals.outputDir, "output-dir", "./triage-notes", "directory to write paired .md and .json artifacts")
	root.PersistentFlags().BoolVar(&globals.quiet, "quiet", false, "suppress non-essential stderr output")
	root.PersistentFlags().StringVar(&globals.configPath, "config", "", "optional path to a config file (unused in spike)")

	root.AddCommand(newVersionCmd())
	root.AddCommand(newDoctorCmd())
	root.AddCommand(newInvestigateCmd())
	root.AddCommand(newTriageCmd())
	root.AddCommand(newWatchCmd())

	return root
}

// Execute runs the CLI and returns a process exit code.
func Execute() int {
	if err := newRoot().Execute(); err != nil {
		fmt.Fprintln(os.Stderr, "error:", err)
		return 1
	}
	return 0
}
