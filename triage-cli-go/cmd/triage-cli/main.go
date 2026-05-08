// Command triage-cli is the entry point for the Go spike of the
// guided Zendesk ticket investigation assistant.
package main

import (
	"os"

	"github.com/midwestman35/triage-cli-go/internal/cli"
)

func main() {
	os.Exit(cli.Execute())
}
