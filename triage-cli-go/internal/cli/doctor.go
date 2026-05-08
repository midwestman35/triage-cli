package cli

import (
	"fmt"
	"io"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
)

func newDoctorCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "doctor",
		Short: "Run environment readiness checks",
		Long:  "doctor inspects environment variables and writability of the directories triage-cli depends on.",
		RunE: func(cmd *cobra.Command, _ []string) error {
			out := cmd.ErrOrStderr()
			fmt.Fprintln(out, "triage-cli doctor")

			critical := 0

			// Zendesk env vars (warn only)
			for _, key := range []string{"ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN"} {
				mark := "✓"
				note := "set"
				if os.Getenv(key) == "" {
					mark = "−"
					note = "not set (required for live mode; --mock works without it)"
				}
				fmt.Fprintf(out, "  %s %s: %s\n", mark, key, note)
			}

			// Datadog env vars (info only)
			for _, key := range []string{"DATADOG_API_KEY", "DATADOG_APP_KEY"} {
				mark := "✓"
				note := "set"
				if os.Getenv(key) == "" {
					mark = "−"
					note = "not set (optional; Datadog evidence not implemented in spike)"
				}
				fmt.Fprintf(out, "  %s %s: %s\n", mark, key, note)
			}

			// Output dir
			outputDir := globals.outputDir
			if outputDir == "" {
				outputDir = "./triage-notes"
			}
			if err := ensureWritable(out, outputDir, "output dir"); err != nil {
				critical++
			}

			// Watcher state dir
			stateDir := filepath.Join(outputDir, ".watcher")
			if err := ensureWritable(out, stateDir, "watcher state dir"); err != nil {
				critical++
			}

			if critical > 0 {
				return fmt.Errorf("%d critical doctor check(s) failed", critical)
			}
			fmt.Fprintln(out, "OK")
			return nil
		},
	}
}

func ensureWritable(out io.Writer, dir, label string) error {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		fmt.Fprintf(out, "  ✗ %s (%s): %v\n", label, dir, err)
		return err
	}
	probe := filepath.Join(dir, ".triage-cli-doctor")
	f, err := os.Create(probe)
	if err != nil {
		fmt.Fprintf(out, "  ✗ %s (%s): %v\n", label, dir, err)
		return err
	}
	_ = f.Close()
	_ = os.Remove(probe)
	fmt.Fprintf(out, "  ✓ %s: %s (writable)\n", label, dir)
	return nil
}
