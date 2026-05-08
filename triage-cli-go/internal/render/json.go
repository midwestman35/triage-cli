package render

import (
	"encoding/json"

	"github.com/midwestman35/triage-cli-go/internal/model"
)

// JSON pretty-prints a TriageReport with two-space indentation.
func JSON(report model.TriageReport) ([]byte, error) {
	return json.MarshalIndent(report, "", "  ")
}
