// Package zendesk provides ticket fetching and ticket-ID parsing.
package zendesk

import (
	"errors"
	"fmt"
	"regexp"
	"strconv"
	"strings"
)

// ticketURLPattern matches the ticket id segment in either
// `/tickets/<id>` or `/agent/tickets/<id>` URLs.
var ticketURLPattern = regexp.MustCompile(`/tickets/(\d+)`)

// rawIntPattern matches a string that is purely an integer.
var rawIntPattern = regexp.MustCompile(`^\d+$`)

// ParseTicketID extracts a Zendesk ticket ID from either a raw integer
// string or a URL containing `/tickets/<id>` (or `/agent/tickets/<id>`).
// Surrounding whitespace is trimmed. An error is returned for empty or
// unrecognized input.
func ParseTicketID(s string) (int64, error) {
	trimmed := strings.TrimSpace(s)
	if trimmed == "" {
		return 0, errors.New("ticket id is empty")
	}

	if rawIntPattern.MatchString(trimmed) {
		n, err := strconv.ParseInt(trimmed, 10, 64)
		if err != nil {
			return 0, fmt.Errorf("parse ticket id %q: %w", trimmed, err)
		}
		return n, nil
	}

	if matches := ticketURLPattern.FindStringSubmatch(trimmed); len(matches) == 2 {
		n, err := strconv.ParseInt(matches[1], 10, 64)
		if err != nil {
			return 0, fmt.Errorf("parse ticket id from url %q: %w", trimmed, err)
		}
		return n, nil
	}

	return 0, fmt.Errorf("could not extract ticket id from %q", trimmed)
}
