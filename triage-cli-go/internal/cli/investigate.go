package cli

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"time"

	"github.com/spf13/cobra"

	"github.com/midwestman35/triage-cli-go/internal/assessment"
	"github.com/midwestman35/triage-cli-go/internal/config"
	"github.com/midwestman35/triage-cli-go/internal/investigation"
	"github.com/midwestman35/triage-cli-go/internal/render"
	"github.com/midwestman35/triage-cli-go/internal/store"
	"github.com/midwestman35/triage-cli-go/internal/zendesk"
)

type investigateFlags struct {
	mock          bool
	json          bool
	evidencePaths []string
	timeout       time.Duration
}

func newInvestigateCmd() *cobra.Command {
	f := &investigateFlags{}
	cmd := &cobra.Command{
		Use:   "investigate <ticket>",
		Short: "Guided investigation of a single Zendesk ticket",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return runPipeline(cmd.Context(), args[0], f, true)
		},
	}
	cmd.Flags().BoolVar(&f.mock, "mock", false, "use the mock Zendesk fetcher (no network, no env vars)")
	cmd.Flags().BoolVar(&f.json, "json", false, "emit JSON to stdout instead of Markdown")
	cmd.Flags().StringArrayVar(&f.evidencePaths, "evidence", nil, "path to a local evidence file (repeatable)")
	cmd.Flags().DurationVar(&f.timeout, "timeout", 30*time.Second, "per-request HTTP timeout for the live Zendesk client")
	return cmd
}

// runPipeline is shared by investigate and triage. guided=true emits
// stderr phase headers and includes the timeline section in Markdown.
func runPipeline(ctx context.Context, idArg string, f *investigateFlags, guided bool) error {
	if ctx == nil {
		ctx = context.Background()
	}
	id, err := zendesk.ParseTicketID(idArg)
	if err != nil {
		return err
	}

	fetcher, err := buildFetcher(f)
	if err != nil {
		return err
	}

	deps := investigation.Deps{
		Fetcher:  fetcher,
		Assessor: assessment.StubAssessor{},
		Now:      func() time.Time { return time.Now().UTC() },
	}
	report, err := investigation.Run(ctx, deps, investigation.RunOpts{
		TicketID:      id,
		EvidencePaths: f.evidencePaths,
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

	if f.json {
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

// buildFetcher returns the appropriate Zendesk fetcher based on flags.
// In --mock mode it never touches the environment. In live mode it
// loads ZENDESK_* env vars and returns a clear error if any are missing.
func buildFetcher(f *investigateFlags) (zendesk.Fetcher, error) {
	if f.mock {
		return zendesk.NewMockFetcher("testdata/tickets"), nil
	}
	cfg, err := config.LoadZendesk()
	if err != nil {
		return nil, fmt.Errorf("zendesk config: %w", err)
	}
	hf := zendesk.NewHTTPFetcher(cfg)
	if f.timeout > 0 {
		hf.SetHTTPClient(&http.Client{Timeout: f.timeout})
	}
	return hf, nil
}
