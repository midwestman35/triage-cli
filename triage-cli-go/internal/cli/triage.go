package cli

import (
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
	addCommonInvestigateFlags(cmd, f)
	return cmd
}
