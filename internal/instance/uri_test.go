package instance

// URI allowlist tests.

import "testing"

func TestForwardableURI(t *testing.T) {
	t.Parallel()

	yes := []string{"http://example.com", "https://example.com/x", "HTTP://X", "HTTPS://x"}
	for _, u := range yes {
		if !ForwardableURI(u) {
			t.Errorf("ForwardableURI(%q)=false", u)
		}
		if err := CheckHostOpenURI(u); err != nil {
			t.Errorf("CheckHostOpenURI(%q)=%v", u, err)
		}
	}
	no := []string{"file:///tmp/x", "mailto:a@b.c", "ftp://x", "about:blank", "not a uri", ""}
	for _, u := range no {
		if ForwardableURI(u) {
			t.Errorf("ForwardableURI(%q)=true", u)
		}
		if err := CheckHostOpenURI(u); err == nil {
			t.Errorf("CheckHostOpenURI(%q) succeeded", u)
		}
	}
}
