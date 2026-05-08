// Package evidence converts ticket comments, attachments, local files,
// and pasted text into normalized model.Evidence values, and constructs
// the merged investigation timeline.
package evidence

const excerptLimit = 500

// excerpt returns the first n runes of s with a trailing ellipsis when
// truncation occurred. Treating the input as runes keeps multibyte
// characters from being sliced mid-codepoint.
func excerpt(s string, n int) string {
	r := []rune(s)
	if len(r) <= n {
		return s
	}
	return string(r[:n]) + "..."
}
