package instance

// OpenURI scheme policy.

import (
	"fmt"
	"net/url"
	"strings"
)

// ForwardableURI reports whether uri should leave the sandbox via the host.
func ForwardableURI(raw string) bool {
	u, err := url.Parse(raw)
	if err != nil || u.Scheme == "" {
		return false
	}
	switch strings.ToLower(u.Scheme) {
	case "http", "https":
		return true
	default:
		return false
	}
}

// CheckHostOpenURI refuses non-http(s) URIs at the daemon.
func CheckHostOpenURI(raw string) error {
	if !ForwardableURI(raw) {
		return fmt.Errorf("refused open_uri scheme")
	}
	return nil
}
