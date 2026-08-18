package instance

// systemd escape and identity tests.

import (
	"os/exec"
	"strings"
	"testing"
)

func TestEscapeMatchesSystemd(t *testing.T) {
	t.Parallel()

	cases := []string{
		"firefox/default",
		"foo-bar/work",
		"app/default",
		"A._b/c-d",
	}
	esc, err := exec.LookPath("systemd-escape")
	hasSystemd := err == nil
	for _, in := range cases {
		got := Escape(in)
		if hasSystemd {
			out, err := exec.Command(esc, in).Output()
			if err != nil {
				t.Fatal(err)
			}
			want := strings.TrimSpace(string(out))
			if got != want {
				t.Errorf("Escape(%q)=%q, systemd-escape=%q", in, got, want)
			}
		}
		back, err := Unescape(got)
		if err != nil {
			t.Fatalf("Unescape(%q): %v", got, err)
		}
		if back != in {
			t.Errorf("Unescape(Escape(%q))=%q", in, back)
		}
	}
}

func TestSplitIdentityAndSocketPath(t *testing.T) {
	t.Parallel()

	sandbox, inst, err := SplitIdentity("firefox/default")
	if err != nil || sandbox != "firefox" || inst != "default" {
		t.Fatalf("SplitIdentity: %s %s %v", sandbox, inst, err)
	}
	if _, _, err := SplitIdentity("firefox"); err == nil {
		t.Fatal("expected error")
	}

	escaped := Escape("firefox/default")
	path := SocketPath("/run/user/1000", escaped)
	if err := CheckSocketPath(path); err != nil {
		t.Fatal(err)
	}
	if err := CheckSocketPath(strings.Repeat("a", SunPathMax)); err == nil {
		t.Fatal("expected sun_path error")
	}
	sock, svc := UnitNames(escaped)
	if sock != "psboxd@"+escaped+".socket" || svc != "psboxd@"+escaped+".service" {
		t.Fatalf("units %s %s", sock, svc)
	}
}
