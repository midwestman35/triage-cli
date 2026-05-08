package zendesk

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"time"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// fixedNow is a deterministic timestamp used by the built-in fallback
// fixture so test output is stable across runs.
var fixedNow = time.Date(2026, 5, 8, 14, 30, 0, 0, time.UTC)

// MockFetcher returns ticket fixtures from a directory of JSON files,
// falling back to a synthesized ticket when no fixture is found.
type MockFetcher struct {
	// FixturesDir is the directory containing `<id>.json` fixtures.
	// If empty, every fetch uses the built-in fallback.
	FixturesDir string
}

// NewMockFetcher constructs a MockFetcher rooted at fixturesDir.
func NewMockFetcher(fixturesDir string) *MockFetcher {
	return &MockFetcher{FixturesDir: fixturesDir}
}

// FetchTicket returns a fixture for the given ticket id. If no fixture
// is configured or the file is missing, a deterministic synthetic
// ticket is returned.
func (m *MockFetcher) FetchTicket(_ context.Context, id int64) (model.Ticket, error) {
	if m.FixturesDir != "" {
		path := filepath.Join(m.FixturesDir, fmt.Sprintf("%d.json", id))
		raw, err := os.ReadFile(path)
		switch {
		case err == nil:
			var t model.Ticket
			if err := json.Unmarshal(raw, &t); err != nil {
				return model.Ticket{}, fmt.Errorf("decode fixture %s: %w", path, err)
			}
			if t.ID == 0 {
				t.ID = id
			}
			return t, nil
		case errors.Is(err, fs.ErrNotExist):
			// fall through to synthesized fixture
		default:
			return model.Ticket{}, fmt.Errorf("read fixture %s: %w", path, err)
		}
	}
	return syntheticTicket(id), nil
}

// syntheticTicket returns a deterministic placeholder ticket for the
// given ID. Timestamps are anchored to a fixed UTC date.
func syntheticTicket(id int64) model.Ticket {
	created := fixedNow.Add(-2 * time.Hour)
	return model.Ticket{
		ID:           id,
		Subject:      "Audio dropping from workstation 3 (mock ticket)",
		Description:  "Dispatcher reports intermittent audio dropouts on workstation 3 starting around 13:15 UTC. Calls connect, but ~5s into the call audio cuts on the dispatcher side. Issue not reproducible on workstation 1 in the same room.",
		RequesterOrg: "Mock County 911",
		Status:       "open",
		Priority:     "high",
		CreatedAt:    created,
		UpdatedAt:    fixedNow,
		Comments: []model.Comment{
			{
				ID:         1001,
				AuthorName: "Dispatcher Lead",
				Public:     true,
				Body:       "We're seeing audio dropouts on WS3 only. Tried headset swap, no change. Other stations are clean.",
				CreatedAt:  created.Add(5 * time.Minute),
			},
			{
				ID:         1002,
				AuthorName: "NOC Engineer",
				Public:     false,
				Body:       "Internal: pulled platform metrics. CPU normal. Saw a brief jitter spike on the SBC at 13:17. Following up.",
				CreatedAt:  created.Add(35 * time.Minute),
			},
			{
				ID:         1003,
				AuthorName: "Dispatcher Lead",
				Public:     true,
				Body:       "Just had another drop on WS3 at ~14:02 local. Caller stayed connected, our side went silent.",
				CreatedAt:  created.Add(75 * time.Minute),
			},
			{
				ID:         1004,
				AuthorName: "NOC Engineer",
				Public:     false,
				Body:       "Internal: confirmed second jitter window 14:02-14:03. Looks station-specific. Asking for headset model + switch port.",
				CreatedAt:  created.Add(95 * time.Minute),
			},
		},
		AttachmentRefs: []model.AttachmentRef{
			{Filename: "ws3-screenshot.png", ContentType: "image/png", SizeBytes: 184320},
			{Filename: "platform-metrics.csv", ContentType: "text/csv", SizeBytes: 12480},
		},
	}
}
