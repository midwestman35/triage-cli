package cli

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/spf13/cobra"

	"github.com/midwestman35/triage-cli-go/internal/config"
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

			zendeskAllSet := true
			for _, key := range []string{"ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN"} {
				mark := "✓"
				note := "set"
				if os.Getenv(key) == "" {
					mark = "−"
					note = "not set (required for live mode; --mock works without it)"
					zendeskAllSet = false
				}
				fmt.Fprintf(out, "  %s %s: %s\n", mark, key, note)
			}

			if zendeskAllSet {
				probeZendesk(cmd.Context(), out)
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

			probeClaudeCLI(cmd.Context(), out)

			outputDir := globals.outputDir
			if outputDir == "" {
				outputDir = "./triage-notes"
			}
			if err := ensureWritable(out, outputDir, "output dir"); err != nil {
				critical++
			}

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

// probeZendesk does a 5s GET against /api/v2/users/me.json and emits a
// single status line. Reachability failures are warnings, not critical.
func probeZendesk(ctx context.Context, out io.Writer) {
	cfg, err := config.LoadZendesk()
	if err != nil {
		fmt.Fprintf(out, "  ✗ zendesk: %v\n", err)
		return
	}
	if ctx == nil {
		ctx = context.Background()
	}
	probeCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()

	url := cfg.BaseURL + "/api/v2/users/me.json"
	req, err := http.NewRequestWithContext(probeCtx, http.MethodGet, url, nil)
	if err != nil {
		fmt.Fprintf(out, "  ✗ zendesk: %v\n", err)
		return
	}
	req.SetBasicAuth(cfg.Email+"/token", cfg.APIToken)
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", "triage-cli/0.1.0-spike (doctor)")

	client := &http.Client{Timeout: 5 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		fmt.Fprintf(out, "  ✗ zendesk: %v\n", err)
		return
	}
	defer resp.Body.Close()

	switch {
	case resp.StatusCode == http.StatusOK:
		var me struct {
			User struct {
				Name  string `json:"name"`
				Email string `json:"email"`
			} `json:"user"`
		}
		if err := json.NewDecoder(resp.Body).Decode(&me); err != nil {
			fmt.Fprintf(out, "  ✓ zendesk: reachable (decode warning: %v)\n", err)
			return
		}
		fmt.Fprintf(out, "  ✓ zendesk: reachable as %s (%s)\n", me.User.Name, me.User.Email)
	case resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden:
		fmt.Fprintf(out, "  ✗ zendesk: authentication failed (check ZENDESK_API_TOKEN)\n")
	default:
		fmt.Fprintf(out, "  ✗ zendesk: HTTP %d\n", resp.StatusCode)
	}
}

// probeClaudeCLI checks whether the `claude` binary is on PATH and
// emits a single status line. Missing claude is informational, not
// critical — the operator can still --no-llm.
func probeClaudeCLI(ctx context.Context, out io.Writer) {
	path, err := exec.LookPath("claude")
	if err != nil {
		fmt.Fprintln(out, "  − claude: not on PATH (LLM-backed assessment unavailable; --no-llm forces the stub)")
		return
	}
	if ctx == nil {
		ctx = context.Background()
	}
	probeCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	cmd := exec.CommandContext(probeCtx, path, "--version")
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		fmt.Fprintf(out, "  ✗ claude: found at %s but --version failed: %v (stderr: %s)\n",
			path, err, strings.TrimSpace(stderr.String()))
		return
	}
	version := strings.TrimSpace(stdout.String())
	if version == "" {
		version = "(empty version output)"
	}
	fmt.Fprintf(out, "  ✓ claude: %s\n", version)
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
