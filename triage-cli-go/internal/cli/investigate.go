package cli

import (
	"context"
	"errors"
	"fmt"
	"os"
	"time"

	"github.com/spf13/cobra"

	"github.com/midwestman35/triage-cli-go/internal/assessment"
	"github.com/midwestman35/triage-cli-go/internal/investigation"
	"github.com/midwestman35/triage-cli-go/internal/render"
	"github.com/midwestman35/triage-cli-go/internal/store"
	"github.com/midwestman35/triage-cli-go/internal/zendesk"
)

type investigateFlags struct {
	mock          bool
	json          bool
	evidencePaths []string
}

func newInvestigateCmd() *cobra.Command {
	f := &investigateFlags{}
	cmd := &cobra.Command{
		Use:   "investigate <ticket>",
		Short: "Guided investigation of a single Zendesk ticket",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return runPipeline(cmd.Context(), args[0], f.mock, f.json, f.evidencePaths, true)
		},
	}
	cmd.Flags().BoolVar(&f.mock, "mock", false, "use the mock Zendesk fetcher (no network, no env vars)")
	cmd.Flags().BoolVar(&f.json, "json", false, "emit JSON to stdout instead of Markdown")
	cmd.Flags().StringArrayVar(&f.evidencePaths, "evidence", nil, "path to a local evidence file (repeatable)")
	return cmd
}

// runPipeline is shared by investigate and triage. guided=true emits
// stderr phase headers and includes the timeline section in Markdown.
func runPipeline(ctx context.Context, idArg string, mock, asJSON bool, evidencePaths []string, guided bool) error {
	if ctx == nil {
		ctx = context.Background()
	}
	id, err := zendesk.ParseTicketID(idArg)
	if err != nil {
		return err
	}

	if !mock {
		if !zendeskCredsConfigured() {
			return errors.New("--mock required (real Zendesk client not implemented in spike); set ZENDESK_SUBDOMAIN/EMAIL/API_TOKEN to opt in once supported")
		}
		// Even with creds, the live fetcher returns an unimplemented
		// error today. Surface that explicitly.
		return errors.New("live Zendesk client not implemented in spike — use --mock")
	}

	deps := investigation.Deps{
		Fetcher:  zendesk.NewMockFetcher("testdata/tickets"),
		Assessor: assessment.StubAssessor{},
		Now:      func() time.Time { return time.Now().UTC() },
	}
	report, err := investigation.Run(ctx, deps, investigation.RunOpts{
		TicketID:      id,
		EvidencePaths: evidencePaths,
		Guided:        guided,
		Quiet:         globals.quiet,
	})
	if err != nil {
		return err
	}

	mdOut := render.Markdown(report, render.MarkdownOpts{IncludeTimeline: guided})
	jsonOut, err := render.JSON(report)
	if err != nil {
		return fmt.Errorf("encode json: %w", err)
	}

	if asJSON {
		fmt.Fprintln(os.Stdout, string(jsonOut))
	} else {
		fmt.Fprint(os.Stdout, mdOut)
	}

	art, err := store.SaveArtifacts(globals.outputDir, report.TicketID, report.GeneratedAt, mdOut, jsonOut)
	if err != nil {
		return fmt.Errorf("save artifacts: %w", err)
	}
	if !globals.quiet {
		render.Status("saved %s", art.MarkdownPath)
		render.Status("saved %s", art.JSONPath)
	}
	return nil
}

func zendeskCredsConfigured() bool {
	for _, k := range []string{"ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN"} {
		if os.Getenv(k) == "" {
			return false
		}
	}
	return true
}
