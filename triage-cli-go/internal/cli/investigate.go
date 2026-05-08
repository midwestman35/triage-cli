package cli

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"os"
	"time"

	"github.com/spf13/cobra"

	"github.com/midwestman35/triage-cli-go/internal/assessment"
	"github.com/midwestman35/triage-cli-go/internal/config"
	"github.com/midwestman35/triage-cli-go/internal/investigation"
	"github.com/midwestman35/triage-cli-go/internal/model"
	"github.com/midwestman35/triage-cli-go/internal/render"
	"github.com/midwestman35/triage-cli-go/internal/store"
	"github.com/midwestman35/triage-cli-go/internal/zendesk"
)

type investigateFlags struct {
	mock          bool
	json          bool
	evidencePaths []string
	timeout       time.Duration
	noLLM         bool
	llmModel      string
	llmVerbose    bool
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
	addCommonInvestigateFlags(cmd, f)
	return cmd
}

// addCommonInvestigateFlags is shared by `investigate` and `triage`.
func addCommonInvestigateFlags(cmd *cobra.Command, f *investigateFlags) {
	cmd.Flags().BoolVar(&f.mock, "mock", false, "use the mock Zendesk fetcher (no network, no env vars)")
	cmd.Flags().BoolVar(&f.json, "json", false, "emit JSON to stdout instead of Markdown")
	cmd.Flags().StringArrayVar(&f.evidencePaths, "evidence", nil, "path to a local evidence file (repeatable)")
	cmd.Flags().DurationVar(&f.timeout, "timeout", 30*time.Second, "per-request HTTP timeout for the live Zendesk client")
	cmd.Flags().BoolVar(&f.noLLM, "no-llm", false, "skip the claude CLI; use the deterministic stub assessor")
	cmd.Flags().StringVar(&f.llmModel, "llm-model", "", "model passed to claude CLI (e.g. claude-sonnet-4-6); empty = CLI default")
	cmd.Flags().BoolVar(&f.llmVerbose, "llm-verbose", false, "mirror claude CLI stderr to our stderr")
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
		Assessor: selectAssessor(f),
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

// selectAssessor picks the Assessor for this run based on flags.
//
// --no-llm forces the deterministic stub. Otherwise we wrap the
// claude CLI assessor in a fallback that switches to the stub if
// (and only if) the binary is missing on PATH. Any other claude
// failure surfaces to the operator.
func selectAssessor(f *investigateFlags) assessment.Assessor {
	if f.noLLM {
		return assessment.StubAssessor{}
	}
	cli := assessment.NewClaudeCLIAssessor(assessment.ClaudeCLIOptions{
		Model:   f.llmModel,
		Verbose: f.llmVerbose,
	})
	return &fallbackAssessor{primary: cli, fallback: assessment.StubAssessor{}}
}

// fallbackAssessor delegates to primary; on ErrClaudeNotFound it
// emits a stderr warning and silently falls back to the stub.
// All other primary errors are surfaced unchanged.
type fallbackAssessor struct {
	primary  assessment.Assessor
	fallback assessment.Assessor
}

func (f *fallbackAssessor) Assess(ctx context.Context, session model.InvestigationSession) (model.Assessment, error) {
	a, err := f.primary.Assess(ctx, session)
	if err == nil {
		return a, nil
	}
	if errors.Is(err, assessment.ErrClaudeNotFound) {
		fmt.Fprintln(os.Stderr, "→ claude CLI not found on PATH; falling back to deterministic stub assessor (use --no-llm to silence)")
		return f.fallback.Assess(ctx, session)
	}
	return model.Assessment{}, err
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
