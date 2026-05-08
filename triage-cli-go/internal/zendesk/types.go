package zendesk

import (
	"strconv"
	"time"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// apiTicket mirrors the relevant subset of Zendesk's ticket payload.
type apiTicket struct {
	ID          int64     `json:"id"`
	Subject     string    `json:"subject"`
	Description string    `json:"description"`
	Status      string    `json:"status"`
	Priority    string    `json:"priority"`
	RequesterID int64     `json:"requester_id"`
	CreatedAt   time.Time `json:"created_at"`
	UpdatedAt   time.Time `json:"updated_at"`
}

// apiTicketResponse wraps the GET /tickets/<id>.json response.
// `include=users` populates the Users array for author lookup.
type apiTicketResponse struct {
	Ticket apiTicket `json:"ticket"`
	Users  []apiUser `json:"users"`
}

// apiUser is the subset we need to resolve comment author names and the
// requester's organization.
type apiUser struct {
	ID             int64  `json:"id"`
	Name           string `json:"name"`
	Email          string `json:"email"`
	OrganizationID int64  `json:"organization_id"`
}

// apiUserResponse wraps GET /users/<id>.json.
type apiUserResponse struct {
	User apiUser `json:"user"`
}

// apiOrgResponse wraps GET /organizations/<id>.json.
type apiOrgResponse struct {
	Organization struct {
		ID   int64  `json:"id"`
		Name string `json:"name"`
	} `json:"organization"`
}

// apiAttachment is a single attachment on a comment.
type apiAttachment struct {
	FileName    string `json:"file_name"`
	ContentType string `json:"content_type"`
	ContentURL  string `json:"content_url"`
	Size        int64  `json:"size"`
}

// apiComment is a single Zendesk ticket comment.
type apiComment struct {
	ID          int64           `json:"id"`
	AuthorID    int64           `json:"author_id"`
	Public      bool            `json:"public"`
	Body        string          `json:"body"`
	CreatedAt   time.Time       `json:"created_at"`
	Attachments []apiAttachment `json:"attachments"`
}

// apiCommentsResponse wraps GET /tickets/<id>/comments.json. NextPage is
// nil when there are no more pages.
type apiCommentsResponse struct {
	Comments []apiComment `json:"comments"`
	NextPage *string      `json:"next_page"`
	Users    []apiUser    `json:"users"`
}

// mapTicket converts the Zendesk wire types into the project's domain
// model. orgName may be "" when org resolution failed or the requester
// has no organization.
func mapTicket(t apiTicket, comments []apiComment, users []apiUser, orgName string) model.Ticket {
	userByID := make(map[int64]apiUser, len(users))
	for _, u := range users {
		userByID[u.ID] = u
	}

	mapped := model.Ticket{
		ID:           t.ID,
		Subject:      t.Subject,
		Description:  t.Description,
		Status:       t.Status,
		Priority:     t.Priority,
		RequesterOrg: orgName,
		CreatedAt:    t.CreatedAt,
		UpdatedAt:    t.UpdatedAt,
	}

	for _, c := range comments {
		author := authorName(c.AuthorID, userByID)
		mapped.Comments = append(mapped.Comments, model.Comment{
			ID:         c.ID,
			AuthorName: author,
			Public:     c.Public,
			Body:       c.Body,
			CreatedAt:  c.CreatedAt,
		})
		for _, a := range c.Attachments {
			mapped.AttachmentRefs = append(mapped.AttachmentRefs, model.AttachmentRef{
				Filename:    a.FileName,
				ContentType: a.ContentType,
				SizeBytes:   a.Size,
				URL:         a.ContentURL,
			})
		}
	}

	return mapped
}

// authorName resolves an author_id against the merged users map, falling
// back to a stable placeholder when the user is not present.
func authorName(id int64, users map[int64]apiUser) string {
	if u, ok := users[id]; ok && u.Name != "" {
		return u.Name
	}
	return "user_" + strconv.FormatInt(id, 10)
}
