// Package config loads runtime configuration from environment variables.
package config

import (
	"fmt"
	"os"
	"sort"
	"strings"
)

// Zendesk holds the configuration needed to talk to the Zendesk REST API.
type Zendesk struct {
	// Subdomain is the Zendesk subdomain (e.g. "carbyne" -> https://carbyne.zendesk.com).
	Subdomain string
	// Email is the Zendesk user email used as the basic-auth identifier.
	Email string
	// APIToken is the Zendesk API token (NOT a password). Combined with
	// Email as `<email>/token:<api_token>` per Zendesk's documented scheme.
	APIToken string
	// BaseURL is the computed base URL: https://<subdomain>.zendesk.com.
	BaseURL string
}

// LoadZendesk reads ZENDESK_SUBDOMAIN, ZENDESK_EMAIL, and ZENDESK_API_TOKEN
// from the environment. Returns an error listing all missing variables when
// any are absent or empty after whitespace trimming. The subdomain is
// normalized — any leading scheme and trailing slashes are stripped — so
// users may paste a full URL.
func LoadZendesk() (Zendesk, error) {
	keys := []string{"ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN"}
	values := make(map[string]string, len(keys))
	var missing []string
	for _, k := range keys {
		v := strings.TrimSpace(os.Getenv(k))
		if v == "" {
			missing = append(missing, k)
			continue
		}
		values[k] = v
	}
	if len(missing) > 0 {
		sort.Strings(missing)
		return Zendesk{}, fmt.Errorf(
			"missing required environment variable(s): %s",
			strings.Join(missing, ", "),
		)
	}

	subdomain := normalizeSubdomain(values["ZENDESK_SUBDOMAIN"])
	if subdomain == "" {
		return Zendesk{}, fmt.Errorf(
			"ZENDESK_SUBDOMAIN is empty after normalization (got %q)",
			values["ZENDESK_SUBDOMAIN"],
		)
	}

	return Zendesk{
		Subdomain: subdomain,
		Email:     values["ZENDESK_EMAIL"],
		APIToken:  values["ZENDESK_API_TOKEN"],
		BaseURL:   fmt.Sprintf("https://%s.zendesk.com", subdomain),
	}, nil
}

// normalizeSubdomain strips any scheme, trailing slashes, and a trailing
// `.zendesk.com` suffix so callers can paste either "carbyne" or
// "https://carbyne.zendesk.com/" and get the same result.
func normalizeSubdomain(raw string) string {
	s := strings.TrimSpace(raw)
	s = strings.TrimPrefix(s, "https://")
	s = strings.TrimPrefix(s, "http://")
	s = strings.TrimRight(s, "/")
	s = strings.TrimSuffix(s, ".zendesk.com")
	s = strings.TrimRight(s, "/")
	return s
}
