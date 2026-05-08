package zendesk

import (
	"context"
	"errors"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// Fetcher fetches a Zendesk ticket by ID. Implementations may hit the
// live API, return mock fixtures, or read from local storage.
type Fetcher interface {
	FetchTicket(ctx context.Context, id int64) (model.Ticket, error)
}

// HTTPFetcher is a placeholder for the live Zendesk HTTP client.
// The spike does not implement live fetching — see HANDOFF for next steps.
type HTTPFetcher struct {
	Subdomain string
	Email     string
	APIToken  string
}

// FetchTicket is not implemented in the spike. The mock fetcher is used
// in --mock mode; live mode returns an explicit error so the CLI can
// surface a clear message.
//
// TODO(next-agent): implement Zendesk REST API client behind this method,
// matching the Python pipeline's ticket fetch behavior.
func (f *HTTPFetcher) FetchTicket(_ context.Context, _ int64) (model.Ticket, error) {
	return model.Ticket{}, errors.New("zendesk HTTPFetcher not implemented in spike")
}
