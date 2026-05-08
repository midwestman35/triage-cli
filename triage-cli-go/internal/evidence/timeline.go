package evidence

import (
	"fmt"
	"sort"
	"strconv"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// BuildTimeline merges ticket and evidence into a chronologically sorted
// timeline. Untimestamped events are appended at the end in their input
// order.
func BuildTimeline(t model.Ticket, ev []model.Evidence) []model.TimelineEvent {
	out := make([]model.TimelineEvent, 0, 1+len(t.Comments)+len(ev))

	created := t.CreatedAt
	out = append(out, model.TimelineEvent{
		Timestamp: &created,
		Source:    "zendesk",
		Kind:      "ticket_created",
		Message:   t.Subject,
		RawRef:    fmt.Sprintf("zendesk:ticket:%d", t.ID),
	})

	for _, c := range t.Comments {
		ts := c.CreatedAt
		visibility := "public"
		if !c.Public {
			visibility = "internal"
		}
		out = append(out, model.TimelineEvent{
			Timestamp: &ts,
			Source:    "zendesk",
			Kind:      "comment",
			Message:   fmt.Sprintf("%s (%s): %s", c.AuthorName, visibility, excerpt(c.Body, 80)),
			RawRef:    "zendesk:comment:" + strconv.FormatInt(c.ID, 10),
		})
	}

	for _, e := range ev {
		if e.Kind == model.EvidenceKindComment {
			continue
		}
		evt := model.TimelineEvent{
			Source:  string(e.Kind),
			Kind:    "evidence_ingested",
			Message: firstNonEmpty(e.Label, e.Source),
			RawRef:  e.Source,
		}
		if !e.CapturedAt.IsZero() {
			c := e.CapturedAt
			evt.Timestamp = &c
		}
		out = append(out, evt)
	}

	sort.SliceStable(out, func(i, j int) bool {
		ti, tj := out[i].Timestamp, out[j].Timestamp
		if ti == nil && tj == nil {
			return false
		}
		if ti == nil {
			return false
		}
		if tj == nil {
			return true
		}
		return ti.Before(*tj)
	})
	return out
}

func firstNonEmpty(a, b string) string {
	if a != "" {
		return a
	}
	return b
}
