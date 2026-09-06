package instance

// Host URI allowlist tests.

import "testing"

func TestHostOpenURIRejectsNonHTTP(t *testing.T) {
	t.Parallel()

	if err := hostOpenURI(t.Context(), "file:///etc/passwd"); err == nil {
		t.Fatal("file: must be rejected before xdg-open")
	}
	if err := hostOpenURI(t.Context(), "mailto:a@b.c"); err == nil {
		t.Fatal("mailto: must be rejected")
	}
}
