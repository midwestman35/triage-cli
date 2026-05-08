package watcher

import (
	"context"
	"fmt"
	"os"
)

// Tick performs one watcher iteration. The spike does not poll Zendesk
// — it logs intent to stderr, loads the state file, and persists it
// back so the file shape exists end-to-end.
func Tick(_ context.Context, viewID int64, statePath string) error {
	fmt.Fprintf(os.Stderr,
		"watcher tick: view=%d (would poll Zendesk and triage updated tickets — not implemented in spike)\n",
		viewID,
	)
	state, err := LoadState(statePath)
	if err != nil {
		return fmt.Errorf("load state: %w", err)
	}
	if err := SaveState(statePath, state); err != nil {
		return fmt.Errorf("save state: %w", err)
	}
	return nil
}
