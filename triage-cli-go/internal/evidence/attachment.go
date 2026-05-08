package evidence

import (
	"time"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// FromAttachmentRefs converts attachment metadata into evidence entries.
// The spike does not download attachments; only metadata is recorded.
func FromAttachmentRefs(refs []model.AttachmentRef) []model.Evidence {
	out := make([]model.Evidence, 0, len(refs))
	now := time.Now().UTC()
	for _, r := range refs {
		out = append(out, model.Evidence{
			Kind:        model.EvidenceKindAttachment,
			Source:      "zendesk:attachment:" + r.Filename,
			Label:       r.Filename,
			SizeBytes:   r.SizeBytes,
			ContentType: r.ContentType,
			CapturedAt:  now,
		})
	}
	return out
}
