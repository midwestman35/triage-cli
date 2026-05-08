package assessment

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// ErrClaudeNotFound is returned when the `claude` binary is not on
// PATH (or cannot be exec'd). Callers can errors.Is against this to
// fall back to a stub assessor.
var ErrClaudeNotFound = errors.New("claude CLI not found on PATH")

// ExecFunc runs a command and returns (stdout, stderr, error). It is
// injectable so tests can stub the subprocess without spawning one.
//
// Implementations must respect ctx cancellation.
type ExecFunc func(ctx context.Context, name string, args []string, stdin []byte) ([]byte, []byte, error)

// ClaudeCLIOptions configures a ClaudeCLIAssessor.
type ClaudeCLIOptions struct {
	// Binary is the executable name or path. Empty means "claude".
	Binary string
	// Model is passed as `--model <Model>` when non-empty. Empty means
	// the CLI's default.
	Model string
	// Verbose mirrors the CLI's stderr to our stderr.
	Verbose bool
	// Exec lets tests inject a fake subprocess runner. Empty means
	// the default os/exec implementation.
	Exec ExecFunc
}

// ClaudeCLIAssessor is an Assessor that delegates to the local
// `claude` CLI as a subprocess. It preserves the no-API-key UX of
// the Python triage-cli — `claude` inherits the user's OAuth seat.
type ClaudeCLIAssessor struct {
	Binary  string
	Model   string
	Verbose bool
	Exec    ExecFunc
}

// NewClaudeCLIAssessor constructs an assessor with sensible defaults.
func NewClaudeCLIAssessor(opts ClaudeCLIOptions) *ClaudeCLIAssessor {
	binary := opts.Binary
	if binary == "" {
		binary = "claude"
	}
	execFn := opts.Exec
	if execFn == nil {
		execFn = defaultExec(opts.Verbose)
	}
	return &ClaudeCLIAssessor{
		Binary:  binary,
		Model:   opts.Model,
		Verbose: opts.Verbose,
		Exec:    execFn,
	}
}

// claudeJSONWrapper matches `claude -p ... --output-format json`'s
// envelope. Confirmed shape (claude 2.x):
//
//	{"type":"result","subtype":"success","is_error":false,
//	 "result":"<the model's text>", ...}
//
// Other top-level fields (duration_ms, usage, modelUsage, session_id)
// are deliberately ignored.
type claudeJSONWrapper struct {
	Type     string `json:"type"`
	Subtype  string `json:"subtype"`
	IsError  bool   `json:"is_error"`
	Result   string `json:"result"`
	APIError string `json:"api_error_status"`
}

// Assess builds a structured prompt, invokes the claude CLI, and
// parses the returned JSON into a model.Assessment. See package docs
// for error mapping.
func (c *ClaudeCLIAssessor) Assess(ctx context.Context, session model.InvestigationSession) (model.Assessment, error) {
	prompt := BuildPrompt(session)

	args := []string{"-p", prompt, "--output-format", "json"}
	if c.Model != "" {
		args = append(args, "--model", c.Model)
	}

	stdout, stderr, err := c.Exec(ctx, c.Binary, args, nil)
	if err != nil {
		if ctx.Err() != nil {
			return model.Assessment{}, ctx.Err()
		}
		if isNotFoundErr(err) {
			return model.Assessment{}, fmt.Errorf("%w: %v", ErrClaudeNotFound, err)
		}
		return model.Assessment{}, fmt.Errorf("claude CLI exit: %v\nstderr: %s", err, truncate(string(stderr), 1024))
	}

	var wrapper claudeJSONWrapper
	if err := json.Unmarshal(stdout, &wrapper); err != nil {
		return model.Assessment{}, fmt.Errorf("claude CLI response parse: %w (raw: %s)", err, truncate(string(stdout), 1024))
	}
	if wrapper.IsError || (wrapper.Subtype != "" && wrapper.Subtype != "success") {
		return model.Assessment{}, fmt.Errorf("claude CLI reported error: subtype=%q api_error=%q result=%s",
			wrapper.Subtype, wrapper.APIError, truncate(wrapper.Result, 512))
	}

	jsonText, err := extractJSONObject(wrapper.Result)
	if err != nil {
		return model.Assessment{}, fmt.Errorf("claude CLI response parse: %w (raw: %s)", err, truncate(wrapper.Result, 1024))
	}

	var assessment model.Assessment
	if err := json.Unmarshal([]byte(jsonText), &assessment); err != nil {
		return model.Assessment{}, fmt.Errorf("claude CLI assessment unmarshal: %w (raw: %s)", err, truncate(jsonText, 1024))
	}

	if err := validateAssessment(&assessment); err != nil {
		return model.Assessment{}, fmt.Errorf("claude CLI assessment invalid: %w", err)
	}
	return assessment, nil
}

// extractJSONObject strips markdown fences and surrounding prose to
// recover the first balanced top-level {...} block.
func extractJSONObject(s string) (string, error) {
	s = strings.TrimSpace(s)
	if s == "" {
		return "", errors.New("empty result")
	}
	// Strip ```json ... ``` or ``` ... ``` fences.
	if strings.HasPrefix(s, "```") {
		// Drop the first line (```json or ```)
		if nl := strings.IndexByte(s, '\n'); nl >= 0 {
			s = s[nl+1:]
		}
		if idx := strings.LastIndex(s, "```"); idx >= 0 {
			s = s[:idx]
		}
		s = strings.TrimSpace(s)
	}

	// Fast path: the whole thing is a JSON object.
	if strings.HasPrefix(s, "{") && strings.HasSuffix(s, "}") {
		return s, nil
	}

	// Slow path: find the first balanced {...} block, respecting
	// strings and escapes.
	start := strings.IndexByte(s, '{')
	if start < 0 {
		return "", errors.New("no JSON object found in result")
	}
	depth := 0
	inString := false
	escaped := false
	for i := start; i < len(s); i++ {
		ch := s[i]
		if inString {
			if escaped {
				escaped = false
				continue
			}
			if ch == '\\' {
				escaped = true
				continue
			}
			if ch == '"' {
				inString = false
			}
			continue
		}
		switch ch {
		case '"':
			inString = true
		case '{':
			depth++
		case '}':
			depth--
			if depth == 0 {
				return s[start : i+1], nil
			}
		}
	}
	return "", errors.New("unbalanced JSON object in result")
}

// validateAssessment checks the parsed assessment for required fields
// and a valid Confidence enum value.
func validateAssessment(a *model.Assessment) error {
	switch a.Confidence {
	case model.ConfidenceConfirmed, model.ConfidenceLikely,
		model.ConfidencePossible, model.ConfidenceUnknown:
		// ok
	case "":
		return errors.New("confidence is empty (must be one of confirmed/likely/possible/unknown)")
	default:
		return fmt.Errorf("confidence %q is not one of confirmed/likely/possible/unknown", a.Confidence)
	}
	if strings.TrimSpace(a.Summary) == "" {
		return errors.New("summary is empty")
	}
	if strings.TrimSpace(a.LikelyRootCause) == "" {
		return errors.New("likely_root_cause is empty")
	}
	if strings.TrimSpace(a.SuggestedInternalNote) == "" {
		return errors.New("suggested_internal_note is empty")
	}
	return nil
}

// isNotFoundErr returns true if err indicates the binary is missing.
func isNotFoundErr(err error) bool {
	if err == nil {
		return false
	}
	if errors.Is(err, exec.ErrNotFound) {
		return true
	}
	// os/exec on macOS/linux wraps ENOENT in a *exec.Error.
	var execErr *exec.Error
	if errors.As(err, &execErr) {
		if errors.Is(execErr.Err, exec.ErrNotFound) {
			return true
		}
	}
	// Fallback: PathError with ENOENT.
	var pathErr *os.PathError
	if errors.As(err, &pathErr) {
		return os.IsNotExist(pathErr.Err)
	}
	return false
}

func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	return s[:n] + "...[truncated]"
}

// defaultExec returns an ExecFunc backed by os/exec.CommandContext.
// When verbose is true, the CLI's stderr is mirrored to os.Stderr as
// it streams (in addition to being captured for error reporting).
func defaultExec(verbose bool) ExecFunc {
	return func(ctx context.Context, name string, args []string, stdin []byte) ([]byte, []byte, error) {
		cmd := exec.CommandContext(ctx, name, args...)
		if len(stdin) > 0 {
			cmd.Stdin = bytes.NewReader(stdin)
		}
		var stdoutBuf, stderrBuf bytes.Buffer
		cmd.Stdout = &stdoutBuf
		if verbose {
			cmd.Stderr = io.MultiWriter(&stderrBuf, os.Stderr)
		} else {
			cmd.Stderr = &stderrBuf
		}
		err := cmd.Run()
		return stdoutBuf.Bytes(), stderrBuf.Bytes(), err
	}
}
