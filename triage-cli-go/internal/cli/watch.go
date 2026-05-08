package cli

import (
	"fmt"
	"path/filepath"
	"time"

	"github.com/spf13/cobra"

	"github.com/midwestman35/triage-cli-go/internal/watcher"
)

type watchFlags struct {
	view       int64
	interval   time.Duration
	once       bool
	continuous bool
}

func newWatchCmd() *cobra.Command {
	f := &watchFlags{once: true}
	cmd := &cobra.Command{
		Use:   "watch",
		Short: "Skeleton watcher for a Zendesk view (spike: one-shot only)",
		RunE: func(cmd *cobra.Command, _ []string) error {
			if f.continuous {
				f.once = false
			}
			if f.view == 0 {
				return fmt.Errorf("--view is required")
			}
			statePath := filepath.Join(globals.outputDir, ".watcher", fmt.Sprintf("watcher-state-%d.json", f.view))
			if err := watcher.Tick(cmd.Context(), f.view, statePath); err != nil {
				return err
			}
			if !f.once {
				fmt.Fprintln(cmd.ErrOrStderr(), "→ continuous polling not implemented in spike; exiting after one tick")
			}
			return nil
		},
	}
	cmd.Flags().Int64Var(&f.view, "view", 0, "Zendesk view ID to watch (required)")
	cmd.Flags().DurationVar(&f.interval, "interval", 60*time.Second, "poll interval (unused in spike)")
	cmd.Flags().BoolVar(&f.continuous, "continuous", false, "run continuously (not implemented in spike)")
	cmd.Flags().BoolVar(&f.once, "once", true, "run a single iteration and exit")
	_ = cmd.MarkFlagRequired("view")
	return cmd
}
