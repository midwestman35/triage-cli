package config

import (
	"strings"
	"testing"
)

func TestLoadZendesk_AllPresent(t *testing.T) {
	t.Setenv("ZENDESK_SUBDOMAIN", "carbyne")
	t.Setenv("ZENDESK_EMAIL", "user@example.com")
	t.Setenv("ZENDESK_API_TOKEN", "tok-123")

	cfg, err := LoadZendesk()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if cfg.Subdomain != "carbyne" {
		t.Errorf("subdomain: got %q want %q", cfg.Subdomain, "carbyne")
	}
	if cfg.Email != "user@example.com" {
		t.Errorf("email: got %q", cfg.Email)
	}
	if cfg.APIToken != "tok-123" {
		t.Errorf("api token: got %q", cfg.APIToken)
	}
	if cfg.BaseURL != "https://carbyne.zendesk.com" {
		t.Errorf("base url: got %q", cfg.BaseURL)
	}
}

func TestLoadZendesk_MissingOne(t *testing.T) {
	t.Setenv("ZENDESK_SUBDOMAIN", "carbyne")
	t.Setenv("ZENDESK_EMAIL", "user@example.com")
	t.Setenv("ZENDESK_API_TOKEN", "")

	_, err := LoadZendesk()
	if err == nil {
		t.Fatal("expected error, got nil")
	}
	if !strings.Contains(err.Error(), "ZENDESK_API_TOKEN") {
		t.Errorf("error should mention missing var; got: %v", err)
	}
	if strings.Contains(err.Error(), "ZENDESK_SUBDOMAIN") {
		t.Errorf("error should not mention present var; got: %v", err)
	}
}

func TestLoadZendesk_AllMissing(t *testing.T) {
	t.Setenv("ZENDESK_SUBDOMAIN", "")
	t.Setenv("ZENDESK_EMAIL", "")
	t.Setenv("ZENDESK_API_TOKEN", "")

	_, err := LoadZendesk()
	if err == nil {
		t.Fatal("expected error, got nil")
	}
	for _, key := range []string{"ZENDESK_SUBDOMAIN", "ZENDESK_EMAIL", "ZENDESK_API_TOKEN"} {
		if !strings.Contains(err.Error(), key) {
			t.Errorf("error should mention %s; got: %v", key, err)
		}
	}
}

func TestLoadZendesk_SubdomainStripsHTTPS(t *testing.T) {
	cases := []struct {
		input string
		want  string
	}{
		{"https://carbyne.zendesk.com", "carbyne"},
		{"https://carbyne.zendesk.com/", "carbyne"},
		{"http://carbyne.zendesk.com/", "carbyne"},
		{"carbyne.zendesk.com", "carbyne"},
		{"carbyne", "carbyne"},
		{"  carbyne  ", "carbyne"},
	}
	for _, tc := range cases {
		t.Run(tc.input, func(t *testing.T) {
			t.Setenv("ZENDESK_SUBDOMAIN", tc.input)
			t.Setenv("ZENDESK_EMAIL", "u@e.com")
			t.Setenv("ZENDESK_API_TOKEN", "tok")
			cfg, err := LoadZendesk()
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if cfg.Subdomain != tc.want {
				t.Errorf("subdomain: got %q want %q", cfg.Subdomain, tc.want)
			}
			if cfg.BaseURL != "https://"+tc.want+".zendesk.com" {
				t.Errorf("base url: got %q", cfg.BaseURL)
			}
		})
	}
}

func TestLoadZendesk_TrailingWhitespace(t *testing.T) {
	t.Setenv("ZENDESK_SUBDOMAIN", "  carbyne\n")
	t.Setenv("ZENDESK_EMAIL", " user@example.com ")
	t.Setenv("ZENDESK_API_TOKEN", "\ttok-123 ")

	cfg, err := LoadZendesk()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if cfg.Subdomain != "carbyne" {
		t.Errorf("subdomain: got %q", cfg.Subdomain)
	}
	if cfg.Email != "user@example.com" {
		t.Errorf("email: got %q", cfg.Email)
	}
	if cfg.APIToken != "tok-123" {
		t.Errorf("token: got %q", cfg.APIToken)
	}
}

func TestLoadZendesk_SubdomainEmptyAfterNormalize(t *testing.T) {
	t.Setenv("ZENDESK_SUBDOMAIN", "https://")
	t.Setenv("ZENDESK_EMAIL", "user@example.com")
	t.Setenv("ZENDESK_API_TOKEN", "tok")

	_, err := LoadZendesk()
	if err == nil {
		t.Fatal("expected error, got nil")
	}
}
