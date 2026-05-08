package zendesk

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/midwestman35/triage-cli-go/internal/config"
	"github.com/midwestman35/triage-cli-go/internal/model"
)

// userAgent identifies this CLI to Zendesk.
const userAgent = "triage-cli/0.1.0-spike (go-spike)"

// maxComments caps the number of comments fetched per ticket so a
// runaway pagination loop cannot exhaust memory or call quota.
const maxComments = 500

// errBodyLimit caps how much of an error response body we surface back
// to the user; 1KB is plenty for Zendesk error envelopes.
const errBodyLimit = 1024

// defaultHTTPTimeout is the default per-request timeout for the live
// Zendesk client. Override with HTTPFetcher.SetHTTPClient for tests.
const defaultHTTPTimeout = 30 * time.Second

// Fetcher fetches a Zendesk ticket by ID. Implementations may hit the
// live API, return mock fixtures, or read from local storage.
type Fetcher interface {
	FetchTicket(ctx context.Context, id int64) (model.Ticket, error)
}

// HTTPFetcher is the live Zendesk REST client. It satisfies Fetcher.
type HTTPFetcher struct {
	cfg        config.Zendesk
	httpClient *http.Client
	userAgent  string
}

// NewHTTPFetcher returns a Fetcher backed by Zendesk's REST API. The
// returned fetcher uses a 30-second per-request timeout by default;
// override with SetHTTPClient.
func NewHTTPFetcher(cfg config.Zendesk) *HTTPFetcher {
	return &HTTPFetcher{
		cfg:        cfg,
		httpClient: &http.Client{Timeout: defaultHTTPTimeout},
		userAgent:  userAgent,
	}
}

// SetHTTPClient swaps the underlying http.Client. Intended for tests so
// they can install httptest transports or shortened timeouts.
func (h *HTTPFetcher) SetHTTPClient(c *http.Client) {
	if c != nil {
		h.httpClient = c
	}
}

// FetchTicket loads a single Zendesk ticket and its comments, mapping
// them into the project's domain model. Org resolution is best-effort:
// failures leave RequesterOrg empty rather than failing the whole fetch.
func (h *HTTPFetcher) FetchTicket(ctx context.Context, id int64) (model.Ticket, error) {
	ticketURL := fmt.Sprintf("%s/api/v2/tickets/%d.json?include=users", h.cfg.BaseURL, id)
	var tResp apiTicketResponse
	if err := h.getJSON(ctx, ticketURL, &tResp); err != nil {
		return model.Ticket{}, err
	}

	commentsURL := fmt.Sprintf("%s/api/v2/tickets/%d/comments.json?include=users", h.cfg.BaseURL, id)
	comments, users, err := h.fetchAllComments(ctx, commentsURL)
	if err != nil {
		return model.Ticket{}, err
	}

	// Merge user lists (ticket include + comment include) so author
	// lookup has the widest possible set.
	allUsers := append([]apiUser{}, tResp.Users...)
	allUsers = append(allUsers, users...)

	orgName := h.resolveOrgName(ctx, tResp.Ticket.RequesterID, tResp.Users)
	return mapTicket(tResp.Ticket, comments, allUsers, orgName), nil
}

// fetchAllComments walks the comments.json pagination chain until
// next_page is null or maxComments is reached. The cap is intentionally
// silent on the Ticket struct — we just stop walking and return what we
// have. The error log goes to the caller's wrapped error chain.
func (h *HTTPFetcher) fetchAllComments(ctx context.Context, firstURL string) ([]apiComment, []apiUser, error) {
	var all []apiComment
	var users []apiUser
	next := firstURL
	for next != "" {
		var resp apiCommentsResponse
		if err := h.getJSON(ctx, next, &resp); err != nil {
			return nil, nil, err
		}
		all = append(all, resp.Comments...)
		users = append(users, resp.Users...)
		if len(all) >= maxComments {
			// Truncate to the cap — caller still gets a usable ticket.
			if len(all) > maxComments {
				all = all[:maxComments]
			}
			break
		}
		if resp.NextPage == nil || *resp.NextPage == "" {
			break
		}
		next = *resp.NextPage
	}
	return all, users, nil
}

// resolveOrgName performs the optional requester -> org lookup. Any
// failure (404, auth error, network glitch) returns "" so the calling
// FetchTicket can continue.
func (h *HTTPFetcher) resolveOrgName(ctx context.Context, requesterID int64, includedUsers []apiUser) string {
	if requesterID == 0 {
		return ""
	}
	// First, see if include=users already gave us the requester's org.
	var orgID int64
	for _, u := range includedUsers {
		if u.ID == requesterID && u.OrganizationID != 0 {
			orgID = u.OrganizationID
			break
		}
	}
	// Fallback: explicitly fetch the user record.
	if orgID == 0 {
		userURL := fmt.Sprintf("%s/api/v2/users/%d.json", h.cfg.BaseURL, requesterID)
		var uResp apiUserResponse
		if err := h.getJSON(ctx, userURL, &uResp); err != nil {
			return ""
		}
		orgID = uResp.User.OrganizationID
	}
	if orgID == 0 {
		return ""
	}
	orgURL := fmt.Sprintf("%s/api/v2/organizations/%d.json", h.cfg.BaseURL, orgID)
	var oResp apiOrgResponse
	if err := h.getJSON(ctx, orgURL, &oResp); err != nil {
		return ""
	}
	return oResp.Organization.Name
}

// getJSON GETs a Zendesk URL with basic auth, decodes the response into
// `out`, and wraps every failure mode with context. Context cancellation
// is propagated.
func (h *HTTPFetcher) getJSON(ctx context.Context, rawURL string, out any) error {
	if _, err := url.Parse(rawURL); err != nil {
		return fmt.Errorf("zendesk parse url %q: %w", rawURL, err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, rawURL, nil)
	if err != nil {
		return fmt.Errorf("zendesk new request %s: %w", rawURL, err)
	}
	req.SetBasicAuth(h.cfg.Email+"/token", h.cfg.APIToken)
	req.Header.Set("Accept", "application/json")
	req.Header.Set("User-Agent", h.userAgent)

	resp, err := h.httpClient.Do(req)
	if err != nil {
		// Surface ctx cancellation directly so callers can errors.Is.
		if ctxErr := ctx.Err(); ctxErr != nil {
			return fmt.Errorf("zendesk GET %s: %w", rawURL, ctxErr)
		}
		return fmt.Errorf("zendesk GET %s: %w", rawURL, err)
	}
	defer resp.Body.Close()

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return statusError(rawURL, resp)
	}
	if err := json.NewDecoder(resp.Body).Decode(out); err != nil {
		return fmt.Errorf("zendesk decode response: %w", err)
	}
	return nil
}

// statusError reads up to errBodyLimit of the response body and returns
// a wrapped error annotated with a hint for common Zendesk error codes.
func statusError(rawURL string, resp *http.Response) error {
	body, _ := io.ReadAll(io.LimitReader(resp.Body, errBodyLimit))
	bodyStr := strings.TrimSpace(string(body))

	hint := ""
	switch resp.StatusCode {
	case http.StatusUnauthorized, http.StatusForbidden:
		hint = " (check ZENDESK_EMAIL and ZENDESK_API_TOKEN)"
	case http.StatusNotFound:
		hint = " (ticket not found)"
	case http.StatusTooManyRequests:
		hint = " (rate limited; retry later)"
	}

	return fmt.Errorf("zendesk %s returned %d: %s%s", rawURL, resp.StatusCode, bodyStr, hint)
}

// Compile-time check that HTTPFetcher satisfies Fetcher.
var _ Fetcher = (*HTTPFetcher)(nil)
