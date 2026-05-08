package zendesk

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/midwestman35/triage-cli-go/internal/config"
)

// newTestFetcher returns an HTTPFetcher pointed at the test server.
func newTestFetcher(t *testing.T, srv *httptest.Server) *HTTPFetcher {
	t.Helper()
	cfg := config.Zendesk{
		Subdomain: "test",
		Email:     "user@example.com",
		APIToken:  "tok-secret",
		BaseURL:   srv.URL,
	}
	f := NewHTTPFetcher(cfg)
	f.SetHTTPClient(srv.Client())
	return f
}

func TestFetchTicket_HappyPath(t *testing.T) {
	mux := http.NewServeMux()

	mux.HandleFunc("/api/v2/tickets/12345.json", func(w http.ResponseWriter, r *http.Request) {
		assertAuthAndUA(t, r)
		if got := r.URL.Query().Get("include"); got != "users" {
			t.Errorf("want include=users, got %q", got)
		}
		writeJSON(w, `{
			"ticket": {
				"id": 12345,
				"subject": "Audio drop",
				"description": "WS3 audio drops",
				"status": "open",
				"priority": "high",
				"requester_id": 7001,
				"created_at": "2026-05-08T12:00:00Z",
				"updated_at": "2026-05-08T13:00:00Z"
			},
			"users": [
				{"id": 7001, "name": "Dispatcher Lead", "email": "lead@example.com", "organization_id": 9001}
			]
		}`)
	})

	mux.HandleFunc("/api/v2/tickets/12345/comments.json", func(w http.ResponseWriter, r *http.Request) {
		assertAuthAndUA(t, r)
		writeJSON(w, `{
			"comments": [
				{
					"id": 1, "author_id": 7001, "public": true,
					"body": "We're seeing audio drops",
					"created_at": "2026-05-08T12:05:00Z",
					"attachments": [
						{"file_name": "ws3.png", "content_type": "image/png", "size": 1024, "content_url": "https://cdn.example/ws3.png"}
					]
				},
				{
					"id": 2, "author_id": 9999, "public": false,
					"body": "Internal: jitter spike",
					"created_at": "2026-05-08T12:30:00Z"
				}
			],
			"users": [
				{"id": 9999, "name": "NOC Engineer"}
			],
			"next_page": null
		}`)
	})

	mux.HandleFunc("/api/v2/organizations/9001.json", func(w http.ResponseWriter, r *http.Request) {
		assertAuthAndUA(t, r)
		writeJSON(w, `{"organization": {"id": 9001, "name": "Mock County 911"}}`)
	})

	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)

	f := newTestFetcher(t, srv)
	got, err := f.FetchTicket(context.Background(), 12345)
	if err != nil {
		t.Fatalf("FetchTicket: %v", err)
	}

	if got.ID != 12345 {
		t.Errorf("ID: got %d", got.ID)
	}
	if got.Subject != "Audio drop" {
		t.Errorf("Subject: got %q", got.Subject)
	}
	if got.RequesterOrg != "Mock County 911" {
		t.Errorf("RequesterOrg: got %q", got.RequesterOrg)
	}
	if got.Status != "open" || got.Priority != "high" {
		t.Errorf("status/priority: got %q/%q", got.Status, got.Priority)
	}
	if len(got.Comments) != 2 {
		t.Fatalf("comments: got %d want 2", len(got.Comments))
	}
	if got.Comments[0].AuthorName != "Dispatcher Lead" {
		t.Errorf("comment 0 author: got %q", got.Comments[0].AuthorName)
	}
	if !got.Comments[0].Public {
		t.Errorf("comment 0 should be public")
	}
	if got.Comments[1].AuthorName != "NOC Engineer" {
		t.Errorf("comment 1 author: got %q", got.Comments[1].AuthorName)
	}
	if got.Comments[1].Public {
		t.Errorf("comment 1 should be internal")
	}
	if len(got.AttachmentRefs) != 1 {
		t.Fatalf("attachments: got %d", len(got.AttachmentRefs))
	}
	att := got.AttachmentRefs[0]
	if att.Filename != "ws3.png" || att.ContentType != "image/png" || att.SizeBytes != 1024 {
		t.Errorf("attachment metadata: %+v", att)
	}
	if att.URL != "https://cdn.example/ws3.png" {
		t.Errorf("attachment url: got %q", att.URL)
	}
}

func TestFetchTicket_Pagination(t *testing.T) {
	mux := http.NewServeMux()
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)

	mux.HandleFunc("/api/v2/tickets/1.json", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, `{"ticket": {"id": 1, "subject": "s", "requester_id": 0,
			"created_at": "2026-05-08T00:00:00Z", "updated_at": "2026-05-08T00:00:00Z"},
			"users": []}`)
	})

	mux.HandleFunc("/api/v2/tickets/1/comments.json", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Query().Get("page") == "2" {
			writeJSON(w, `{"comments": [{"id": 2, "author_id": 1, "public": true, "body": "second", "created_at": "2026-05-08T00:01:00Z"}], "users": [], "next_page": null}`)
			return
		}
		nextURL := srv.URL + "/api/v2/tickets/1/comments.json?page=2"
		writeJSON(w, fmt.Sprintf(`{
			"comments": [{"id": 1, "author_id": 1, "public": true, "body": "first", "created_at": "2026-05-08T00:00:00Z"}],
			"users": [{"id": 1, "name": "Alice"}],
			"next_page": %q
		}`, nextURL))
	})

	cfg := config.Zendesk{Subdomain: "test", Email: "u@e.com", APIToken: "t", BaseURL: srv.URL}
	f := NewHTTPFetcher(cfg)
	f.SetHTTPClient(srv.Client())

	got, err := f.FetchTicket(context.Background(), 1)
	if err != nil {
		t.Fatalf("FetchTicket: %v", err)
	}
	if len(got.Comments) != 2 {
		t.Fatalf("expected 2 comments across pages, got %d", len(got.Comments))
	}
	if got.Comments[0].Body != "first" || got.Comments[1].Body != "second" {
		t.Errorf("page order wrong: %+v", got.Comments)
	}
	if got.Comments[0].AuthorName != "Alice" {
		t.Errorf("author from page 1 users not used: %q", got.Comments[0].AuthorName)
	}
}

func TestFetchTicket_401(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusUnauthorized)
		_, _ = io.WriteString(w, `{"error": "unauthorized"}`)
	}))
	t.Cleanup(srv.Close)

	f := newTestFetcher(t, srv)
	_, err := f.FetchTicket(context.Background(), 1)
	if err == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(err.Error(), "401") {
		t.Errorf("error should mention 401: %v", err)
	}
	if !strings.Contains(err.Error(), "ZENDESK_API_TOKEN") {
		t.Errorf("error should hint at token: %v", err)
	}
}

func TestFetchTicket_404(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusNotFound)
		_, _ = io.WriteString(w, `{"error": "RecordNotFound"}`)
	}))
	t.Cleanup(srv.Close)

	f := newTestFetcher(t, srv)
	_, err := f.FetchTicket(context.Background(), 99999)
	if err == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(err.Error(), "404") {
		t.Errorf("error should mention 404: %v", err)
	}
	if !strings.Contains(err.Error(), "ticket not found") {
		t.Errorf("error should hint at not-found: %v", err)
	}
}

func TestFetchTicket_5xx(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusBadGateway)
		_, _ = io.WriteString(w, `gateway down`)
	}))
	t.Cleanup(srv.Close)

	f := newTestFetcher(t, srv)
	_, err := f.FetchTicket(context.Background(), 1)
	if err == nil {
		t.Fatal("expected error")
	}
	if !strings.Contains(err.Error(), "502") {
		t.Errorf("error should mention 502: %v", err)
	}
}

func TestFetchTicket_OrgLookupFailure(t *testing.T) {
	mux := http.NewServeMux()
	mux.HandleFunc("/api/v2/tickets/1.json", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, `{
			"ticket": {"id": 1, "subject": "s", "requester_id": 7001,
				"created_at": "2026-05-08T00:00:00Z", "updated_at": "2026-05-08T00:00:00Z"},
			"users": [{"id": 7001, "name": "Lead", "organization_id": 9001}]
		}`)
	})
	mux.HandleFunc("/api/v2/tickets/1/comments.json", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, `{"comments": [], "users": [], "next_page": null}`)
	})
	mux.HandleFunc("/api/v2/organizations/9001.json", func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusNotFound)
		_, _ = io.WriteString(w, `{"error":"NotFound"}`)
	})
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)

	f := newTestFetcher(t, srv)
	got, err := f.FetchTicket(context.Background(), 1)
	if err != nil {
		t.Fatalf("FetchTicket should succeed despite org lookup failure: %v", err)
	}
	if got.RequesterOrg != "" {
		t.Errorf("RequesterOrg should be empty on org lookup failure, got %q", got.RequesterOrg)
	}
}

func TestFetchTicket_ContextCancellation(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		select {
		case <-r.Context().Done():
			return
		case <-time.After(2 * time.Second):
			writeJSON(w, `{}`)
		}
	}))
	t.Cleanup(srv.Close)

	f := newTestFetcher(t, srv)
	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		time.Sleep(20 * time.Millisecond)
		cancel()
	}()
	_, err := f.FetchTicket(ctx, 1)
	if err == nil {
		t.Fatal("expected error from cancelled context")
	}
	if !errors.Is(err, context.Canceled) {
		t.Errorf("error should wrap context.Canceled: %v", err)
	}
}

func TestFetchTicket_CommentCap(t *testing.T) {
	// Drive pagination so it would loop forever, then assert we stop
	// at maxComments.
	mux := http.NewServeMux()
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)

	mux.HandleFunc("/api/v2/tickets/1.json", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, `{"ticket": {"id": 1, "subject": "s", "requester_id": 0,
			"created_at": "2026-05-08T00:00:00Z", "updated_at": "2026-05-08T00:00:00Z"},
			"users": []}`)
	})

	// Each page returns 50 comments and a next_page that loops back here.
	mux.HandleFunc("/api/v2/tickets/1/comments.json", func(w http.ResponseWriter, _ *http.Request) {
		var b strings.Builder
		b.WriteString(`{"comments":[`)
		for i := 0; i < 50; i++ {
			if i > 0 {
				b.WriteString(",")
			}
			b.WriteString(`{"id":1,"author_id":0,"public":true,"body":"x","created_at":"2026-05-08T00:00:00Z"}`)
		}
		b.WriteString(`],"users":[],"next_page":"`)
		b.WriteString(srv.URL)
		b.WriteString(`/api/v2/tickets/1/comments.json"}`)
		writeJSON(w, b.String())
	})

	cfg := config.Zendesk{Subdomain: "test", Email: "u@e.com", APIToken: "t", BaseURL: srv.URL}
	f := NewHTTPFetcher(cfg)
	f.SetHTTPClient(srv.Client())

	got, err := f.FetchTicket(context.Background(), 1)
	if err != nil {
		t.Fatalf("FetchTicket: %v", err)
	}
	if len(got.Comments) != maxComments {
		t.Errorf("expected comment count to cap at %d, got %d", maxComments, len(got.Comments))
	}
}

// --- helpers ---

func writeJSON(w http.ResponseWriter, body string) {
	w.Header().Set("Content-Type", "application/json")
	_, _ = io.WriteString(w, body)
}

func assertAuthAndUA(t *testing.T, r *http.Request) {
	t.Helper()
	user, pass, ok := r.BasicAuth()
	if !ok {
		t.Errorf("missing basic auth")
		return
	}
	if !strings.HasSuffix(user, "/token") {
		t.Errorf("basic auth user should end with /token, got %q", user)
	}
	if pass == "" {
		t.Errorf("basic auth password empty")
	}
	if ua := r.Header.Get("User-Agent"); !strings.HasPrefix(ua, "triage-cli/") {
		t.Errorf("user-agent: got %q", ua)
	}
}
