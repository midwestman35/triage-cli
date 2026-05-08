package zendesk

import "testing"

func TestParseTicketID(t *testing.T) {
	tests := []struct {
		name    string
		in      string
		want    int64
		wantErr bool
	}{
		{"raw int", "12345", 12345, false},
		{"agent url", "https://example.zendesk.com/agent/tickets/12345", 12345, false},
		{"plain url with query", "https://example.zendesk.com/tickets/67890?foo=bar", 67890, false},
		{"whitespace", "  42  ", 42, false},
		{"empty", "", 0, true},
		{"non-ticket text", "not-a-ticket", 0, true},
		{"url without tickets segment", "https://example.zendesk.com/agent", 0, true},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got, err := ParseTicketID(tc.in)
			if tc.wantErr {
				if err == nil {
					t.Fatalf("expected error for %q, got %d", tc.in, got)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got != tc.want {
				t.Fatalf("got %d, want %d", got, tc.want)
			}
		})
	}
}
