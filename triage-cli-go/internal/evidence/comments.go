package evidence

import (
	"fmt"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// FromComments converts a slice of ticket comments into evidence entries.
// Comment body is truncated to a short excerpt for inclusion in reports.
func FromComments(comments []model.Comment) []model.Evidence {
	out := make([]model.Evidence, 0, len(comments))
	for _, c := range comments {
		visibility := "public"
		if !c.Public {
			visibility = "internal"
		}
		label := fmt.Sprintf("%s comment by %s", visibility, c.AuthorName)
		out = append(out, model.Evidence{
			Kind:       model.EvidenceKindComment,
			Source:     fmt.Sprintf("zendesk:comment:%d", c.ID),
			Label:      label,
			SizeBytes:  int64(len(c.Body)),
			Excerpt:    excerpt(c.Body, excerptLimit),
			CapturedAt: c.CreatedAt,
		})
	}
	return out
}
