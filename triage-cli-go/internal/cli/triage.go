package cli

import (
	"time"

	"github.com/spf13/cobra"
)

func newTriageCmd() *cobra.Command {
	f := &investigateFlags{}
	cmd := &cobra.Command{
		Use:   "triage <ticket>",
		Short: "Non-interactive triage of a single Zendesk ticket",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return runPipeline(cmd.Context(), args[0], f, false)
		},
	}
	cmd.Flags().BoolVar(&f.mock, "mock", false, "use the mock Zendesk fetcher (no network, no env vars)")
	cmd.Flags().BoolVar(&f.json, "json", false, "emit JSON to stdout instead of Markdown")
	cmd.Flags().StringArrayVar(&f.evidencePaths, "evidence", nil, "path to a local evidence file (repeatable)")
	cmd.Flags().DurationVar(&f.timeout, "timeout", 30*time.Second, "per-request HTTP timeout for the live Zendesk client")
	return cmd
}
