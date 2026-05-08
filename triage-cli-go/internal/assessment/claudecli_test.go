package assessment

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// validAssessmentJSON is a minimal Assessment that passes validation.
const validAssessmentJSON = `{
  "summary": "Audio drops on dispatch console.",
  "likely_root_cause": "RTP packet loss between station and core; thin evidence.",
  "confidence": "possible",
  "correlation": ["Reporter says 'audio drops'", "No station logs yet"],
  "unknowns": ["No timestamps"],
  "next_steps": ["Pull station logs"],
  "suggested_internal_note": "Triage notes: awaiting station logs."
}`

func wrap(result string) []byte {
	w := claudeJSONWrapper{
		Type:    "result",
		Subtype: "success",
		Result:  result,
	}
	b, _ := json.Marshal(w)
	return b
}

func sampleSession() model.InvestigationSession {
	return model.InvestigationSession{
		Ticket: model.Ticket{
			ID:          12345,
			Subject:     "Audio dropping at Site X",
			Description: "Dispatcher reports audio drops on calls.",
			CreatedAt:   time.Date(2026, 5, 8, 12, 0, 0, 0, time.UTC),
			Comments: []model.Comment{
				{
					AuthorName: "Reporter",
					Public:     true,
					Body:       "Audio cuts out mid-call.",
					CreatedAt:  time.Date(2026, 5, 8, 12, 5, 0, 0, time.UTC),
				},
			},
		},
	}
}

func TestClaudeCLIAssessor_HappyPath(t *testing.T) {
	t.Parallel()
	fakeExec := func(_ context.Context, _ string, args []string, _ []byte) ([]byte, []byte, error) {
		// Sanity: we should pass the prompt via -p and request JSON output.
		joined := strings.Join(args, " ")
		if !strings.Contains(joined, "--output-format json") {
			t.Errorf("expected --output-format json in args, got %q", joined)
		}
		if args[0] != "-p" {
			t.Errorf("expected first arg to be -p, got %q", args[0])
		}
		return wrap(validAssessmentJSON), nil, nil
	}
	a := NewClaudeCLIAssessor(ClaudeCLIOptions{Exec: fakeExec})
	got, err := a.Assess(context.Background(), sampleSession())
	if err != nil {
		t.Fatalf("Assess returned error: %v", err)
	}
	if got.Confidence != model.ConfidencePossible {
		t.Errorf("confidence = %q, want possible", got.Confidence)
	}
	if got.Summary == "" {
		t.Errorf("summary should not be empty")
	}
	if len(got.Correlation) == 0 {
		t.Errorf("correlation should not be empty")
	}
}

func TestClaudeCLIAssessor_ModelFlagPropagated(t *testing.T) {
	t.Parallel()
	var seenArgs []string
	fakeExec := func(_ context.Context, _ string, args []string, _ []byte) ([]byte, []byte, error) {
		seenArgs = args
		return wrap(validAssessmentJSON), nil, nil
	}
	a := NewClaudeCLIAssessor(ClaudeCLIOptions{Exec: fakeExec, Model: "claude-sonnet-4-6"})
	if _, err := a.Assess(context.Background(), sampleSession()); err != nil {
		t.Fatalf("Assess: %v", err)
	}
	if !strings.Contains(strings.Join(seenArgs, " "), "--model claude-sonnet-4-6") {
		t.Errorf("expected --model flag in args, got %v", seenArgs)
	}
}

func TestClaudeCLIAssessor_CodeFenceWrapped(t *testing.T) {
	t.Parallel()
	fenced := "```json\n" + validAssessmentJSON + "\n```"
	fakeExec := func(_ context.Context, _ string, _ []string, _ []byte) ([]byte, []byte, error) {
		return wrap(fenced), nil, nil
	}
	a := NewClaudeCLIAssessor(ClaudeCLIOptions{Exec: fakeExec})
	got, err := a.Assess(context.Background(), sampleSession())
	if err != nil {
		t.Fatalf("Assess returned error: %v", err)
	}
	if got.Summary == "" {
		t.Errorf("summary should not be empty after fence stripping")
	}
}

func TestClaudeCLIAssessor_LeadingProse(t *testing.T) {
	t.Parallel()
	prosed := "Here is my analysis:\n\n" + validAssessmentJSON + "\n\nLet me know if you have questions."
	fakeExec := func(_ context.Context, _ string, _ []string, _ []byte) ([]byte, []byte, error) {
		return wrap(prosed), nil, nil
	}
	a := NewClaudeCLIAssessor(ClaudeCLIOptions{Exec: fakeExec})
	got, err := a.Assess(context.Background(), sampleSession())
	if err != nil {
		t.Fatalf("Assess returned error: %v", err)
	}
	if got.Confidence != model.ConfidencePossible {
		t.Errorf("confidence = %q, want possible", got.Confidence)
	}
}

func TestClaudeCLIAssessor_InvalidConfidence(t *testing.T) {
	t.Parallel()
	bad := strings.Replace(validAssessmentJSON, `"possible"`, `"medium"`, 1)
	fakeExec := func(_ context.Context, _ string, _ []string, _ []byte) ([]byte, []byte, error) {
		return wrap(bad), nil, nil
	}
	a := NewClaudeCLIAssessor(ClaudeCLIOptions{Exec: fakeExec})
	_, err := a.Assess(context.Background(), sampleSession())
	if err == nil {
		t.Fatalf("expected validation error, got nil")
	}
	if !strings.Contains(err.Error(), "confidence") {
		t.Errorf("error should mention confidence, got %v", err)
	}
}

func TestClaudeCLIAssessor_EmptySummary(t *testing.T) {
	t.Parallel()
	bad := strings.Replace(validAssessmentJSON,
		`"summary": "Audio drops on dispatch console."`,
		`"summary": ""`, 1)
	fakeExec := func(_ context.Context, _ string, _ []string, _ []byte) ([]byte, []byte, error) {
		return wrap(bad), nil, nil
	}
	a := NewClaudeCLIAssessor(ClaudeCLIOptions{Exec: fakeExec})
	_, err := a.Assess(context.Background(), sampleSession())
	if err == nil {
		t.Fatalf("expected validation error, got nil")
	}
	if !strings.Contains(err.Error(), "summary") {
		t.Errorf("error should mention summary, got %v", err)
	}
}

func TestClaudeCLIAssessor_NonZeroExit(t *testing.T) {
	t.Parallel()
	fakeExec := func(_ context.Context, _ string, _ []string, _ []byte) ([]byte, []byte, error) {
		return nil, []byte("auth failed"), errors.New("exit status 1")
	}
	a := NewClaudeCLIAssessor(ClaudeCLIOptions{Exec: fakeExec})
	_, err := a.Assess(context.Background(), sampleSession())
	if err == nil {
		t.Fatalf("expected exit error, got nil")
	}
	if !strings.Contains(err.Error(), "auth failed") {
		t.Errorf("expected stderr in error, got %v", err)
	}
	if errors.Is(err, ErrClaudeNotFound) {
		t.Errorf("non-zero exit should not match ErrClaudeNotFound")
	}
}

func TestClaudeCLIAssessor_BinaryNotFound(t *testing.T) {
	t.Parallel()
	fakeExec := func(_ context.Context, _ string, _ []string, _ []byte) ([]byte, []byte, error) {
		return nil, nil, &exec.Error{Name: "claude", Err: exec.ErrNotFound}
	}
	a := NewClaudeCLIAssessor(ClaudeCLIOptions{Exec: fakeExec})
	_, err := a.Assess(context.Background(), sampleSession())
	if err == nil {
		t.Fatalf("expected ErrClaudeNotFound, got nil")
	}
	if !errors.Is(err, ErrClaudeNotFound) {
		t.Errorf("err = %v; want errors.Is(ErrClaudeNotFound)=true", err)
	}
}

func TestClaudeCLIAssessor_ContextCancellation(t *testing.T) {
	t.Parallel()
	fakeExec := func(ctx context.Context, _ string, _ []string, _ []byte) ([]byte, []byte, error) {
		<-ctx.Done()
		// Simulate the subprocess being killed by the context.
		return nil, nil, ctx.Err()
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel() // cancel before invocation so fakeExec returns immediately
	a := NewClaudeCLIAssessor(ClaudeCLIOptions{Exec: fakeExec})
	_, err := a.Assess(ctx, sampleSession())
	if err == nil {
		t.Fatalf("expected context error, got nil")
	}
	if !errors.Is(err, context.Canceled) {
		t.Errorf("expected context.Canceled, got %v", err)
	}
}

func TestClaudeCLIAssessor_WrapperReportsError(t *testing.T) {
	t.Parallel()
	w := claudeJSONWrapper{
		Type:     "result",
		Subtype:  "error_max_turns",
		IsError:  true,
		APIError: "rate_limited",
		Result:   "",
	}
	b, _ := json.Marshal(w)
	fakeExec := func(_ context.Context, _ string, _ []string, _ []byte) ([]byte, []byte, error) {
		return b, nil, nil
	}
	a := NewClaudeCLIAssessor(ClaudeCLIOptions{Exec: fakeExec})
	_, err := a.Assess(context.Background(), sampleSession())
	if err == nil {
		t.Fatalf("expected wrapper error, got nil")
	}
	if !strings.Contains(err.Error(), "rate_limited") {
		t.Errorf("expected wrapper api_error in message, got %v", err)
	}
}

func TestExtractJSONObject(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		in   string
		want string
	}{
		{"plain", `{"a":1}`, `{"a":1}`},
		{"fenced_json", "```json\n{\"a\":1}\n```", `{"a":1}`},
		{"fenced_bare", "```\n{\"a\":1}\n```", `{"a":1}`},
		{"prose_around", "preamble {\"a\":1} trailer", `{"a":1}`},
		{"nested", `{"a":{"b":2}}`, `{"a":{"b":2}}`},
		{"string_with_brace", `{"a":"}{"}`, `{"a":"}{"}`},
	}
	for _, tc := range cases {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got, err := extractJSONObject(tc.in)
			if err != nil {
				t.Fatalf("err: %v", err)
			}
			if got != tc.want {
				t.Errorf("got %q, want %q", got, tc.want)
			}
		})
	}
}

func TestExtractJSONObject_Errors(t *testing.T) {
	t.Parallel()
	cases := []string{"", "no json here", "{unbalanced"}
	for _, in := range cases {
		if _, err := extractJSONObject(in); err == nil {
			t.Errorf("expected error for %q, got nil", in)
		}
	}
}

func TestBuildPrompt_ContainsKeyFields(t *testing.T) {
	t.Parallel()
	prompt := BuildPrompt(sampleSession())
	for _, want := range []string{
		"Audio dropping at Site X", // subject
		"Audio cuts out mid-call.", // comment body
		`"confidence":             "confirmed" | "likely" | "possible" | "unknown"`,
		"=== TICKET ===",
		"=== EVIDENCE ===",
		"=== TIMELINE ===",
		"Output ONLY the JSON object",
	} {
		if !strings.Contains(prompt, want) {
			t.Errorf("prompt missing %q", want)
		}
	}
}

func TestIsNotFoundErr(t *testing.T) {
	t.Parallel()
	if !isNotFoundErr(&exec.Error{Name: "claude", Err: exec.ErrNotFound}) {
		t.Error("exec.Error{ErrNotFound} should be not-found")
	}
	if !isNotFoundErr(exec.ErrNotFound) {
		t.Error("bare ErrNotFound should be not-found")
	}
	if isNotFoundErr(fmt.Errorf("exit status 1")) {
		t.Error("exit status err should not be not-found")
	}
	if isNotFoundErr(nil) {
		t.Error("nil should not be not-found")
	}
}
