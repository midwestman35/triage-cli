// Package watcher implements the (skeleton) Zendesk-view poll loop and
// its on-disk state file.
package watcher

import (
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
)

// StateVersion is the persisted state-file schema version.
const StateVersion = 1

// State is the persisted watcher state — the set of ticket IDs already
// triaged keyed to their last-seen updated_at timestamp.
type State struct {
	Version int              `json:"version"`
	Triaged map[int64]string `json:"triaged"`
}

// LoadState reads a state file. A missing file returns an empty,
// initialized State (not an error). A version mismatch is an error so
// the caller can decide whether to migrate or refuse.
func LoadState(path string) (State, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return State{Version: StateVersion, Triaged: map[int64]string{}}, nil
		}
		return State{}, fmt.Errorf("read state %s: %w", path, err)
	}
	var s State
	if err := json.Unmarshal(raw, &s); err != nil {
		return State{}, fmt.Errorf("decode state %s: %w", path, err)
	}
	if s.Version == 0 {
		s.Version = StateVersion
	}
	if s.Version != StateVersion {
		return State{}, fmt.Errorf("state version %d does not match expected %d", s.Version, StateVersion)
	}
	if s.Triaged == nil {
		s.Triaged = map[int64]string{}
	}
	return s, nil
}

// SaveState writes state to disk atomically (tempfile + rename) and
// creates the parent directory if needed. The tempfile is cleaned up
// on any failure so it never lingers on disk.
func SaveState(path string, s State) (err error) {
	if err = os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("create state dir: %w", err)
	}
	if s.Version == 0 {
		s.Version = StateVersion
	}
	if s.Triaged == nil {
		s.Triaged = map[int64]string{}
	}
	data, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return fmt.Errorf("encode state: %w", err)
	}
	tmp := path + ".tmp"
	defer func() {
		if err != nil {
			_ = os.Remove(tmp)
		}
	}()
	if err = os.WriteFile(tmp, data, 0o644); err != nil {
		return fmt.Errorf("write %s: %w", tmp, err)
	}
	if err = os.Rename(tmp, path); err != nil {
		return fmt.Errorf("rename %s -> %s: %w", tmp, path, err)
	}
	return nil
}
