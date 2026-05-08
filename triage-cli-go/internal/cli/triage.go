package cli

import (
	"github.com/spf13/cobra"
)

type triageFlags struct {
	mock          bool
	json          bool
	evidencePaths []string
}

func newTriageCmd() *cobra.Command {
	f := &triageFlags{}
	cmd := &cobra.Command{
		Use:   "triage <ticket>",
		Short: "Non-interactive triage of a single Zendesk ticket",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return runPipeline(cmd.Context(), args[0], f.mock, f.json, f.evidencePaths, false)
		},
	}
	cmd.Flags().BoolVar(&f.mock, "mock", false, "use the mock Zendesk fetcher (no network, no env vars)")
	cmd.Flags().BoolVar(&f.json, "json", false, "emit JSON to stdout instead of Markdown")
	cmd.Flags().StringArrayVar(&f.evidencePaths, "evidence", nil, "path to a local evidence file (repeatable)")
	return cmd
}
