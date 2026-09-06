package bwrap

// POSIX shell quoting for --print output.

import "strings"

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
		if !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || strings.ContainsRune("_@%+,-./:", r)) {
			return true
		}
	}
	return false
}
