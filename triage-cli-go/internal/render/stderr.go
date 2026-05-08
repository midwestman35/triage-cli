package render

import (
	"fmt"
	"os"
)

// Status writes a single status line to stderr with an arrow prefix.
// Stdout is reserved for the rendered report; everything else uses
// stderr so output stays pipe-friendly.
func Status(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "→ "+format+"\n", args...)
}
