package bwrap

// POSIX shell quoting for --print output.

import (
	"strings"
	"unicode"
)

// Quote joins argv as a POSIX shell-escaped command line.
func Quote(argv []string) string {
	parts := make([]string, len(argv))
	for i, arg := range argv {
		parts[i] = quoteArg(arg)
	}
	return strings.Join(parts, " ")
}

func quoteArg(s string) string {
	if s != "" && !needsQuote(s) {
		return s
	}
	return "'" + strings.ReplaceAll(s, "'", `'"'"'`) + "'"
}

func needsQuote(s string) bool {
	for _, r := range s {
		if unicode.IsSpace(r) || strings.ContainsRune(`\"'$&*()[]{}|;<>?!~#`, r) {
			return true
		}
	}
	return false
}
