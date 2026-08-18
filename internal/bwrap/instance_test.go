package bwrap

// Instance trailing-command tests.

import (
	"path/filepath"
	"slices"
	"testing"

	"petris.dev/psbox/internal/config"
)

func TestInstanceTrailingSymlinksXDGOpen(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	app := &config.Application{Name: "app"}
	got := InstanceTrailing(app, paths, "/usr/bin/psboxa", 3)
	binDir := XDGOpenBinDir(paths.RuntimeDir)
	shim := filepath.Join(binDir, "xdg-open")

	if containsSeq(got, "--ro-bind", "/usr/bin/psboxa", "/usr/bin/psboxa") {
		t.Fatalf("should not ro-bind agent onto itself: %v", got)
	}
	if !containsSeq(got, "--dir", filepath.Dir(binDir)) {
		t.Fatalf("missing psbox dir: %v", got)
	}
	if !containsSeq(got, "--dir", binDir) {
		t.Fatalf("missing xdg-open bin dir: %v", got)
	}
	if !containsSeq(got, "--symlink", "/usr/bin/psboxa", shim) {
		t.Fatalf("missing xdg-open symlink: %v", got)
	}
	if !containsSeq(got, "--setenv", ControlFDEnv, "3") {
		t.Fatalf("missing control fd: %v", got)
	}
	if got[len(got)-1] != "/usr/bin/psboxa" {
		t.Fatalf("trailing command should be the agent: %v", got)
	}
}

func TestInstanceTrailingBindsAgentOutsideUsr(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	app := &config.Application{Name: "app"}
	got := InstanceTrailing(app, paths, "/opt/psbox/psboxa", 3)
	shim := filepath.Join(XDGOpenBinDir(paths.RuntimeDir), "xdg-open")
	if !containsSeq(got, "--ro-bind", "/opt/psbox/psboxa", "/opt/psbox/psboxa") {
		t.Fatalf("agent outside /usr must be bound: %v", got)
	}
	if !containsSeq(got, "--symlink", "/opt/psbox/psboxa", shim) {
		t.Fatalf("missing xdg-open symlink: %v", got)
	}
}

func TestXDGOpenBinDirEmpty(t *testing.T) {
	t.Parallel()
	if got := XDGOpenBinDir(""); got != "" {
		t.Fatalf("empty runtime dir: %q", got)
	}
}

func TestInstanceTrailingHostURLsOff(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	off := false
	app := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Options: config.ApplicationOptions{HostURLs: &off},
	}}
	got := InstanceTrailing(app, paths, "/usr/bin/psboxa", 3)
	shim := filepath.Join(XDGOpenBinDir(paths.RuntimeDir), "xdg-open")
	if containsSeq(got, "--symlink", "/usr/bin/psboxa", shim) {
		t.Fatalf("host-urls: false must not install xdg-open symlink: %v", got)
	}
	if containsSeq(got, "--dir", XDGOpenBinDir(paths.RuntimeDir)) {
		t.Fatalf("host-urls: false must not create shim dir: %v", got)
	}
	if !containsSeq(got, "--setenv", HostURLsEnv, HostURLsOff) {
		t.Fatalf("host-urls: false should set %s: %v", HostURLsEnv, got)
	}
	if !containsSeq(got, "--setenv", ControlFDEnv, "3") {
		t.Fatalf("missing control fd: %v", got)
	}
}

func TestInstanceTrailingPrivateDBusEnv(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	priv := config.DBusPrivate
	app := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Options: config.ApplicationOptions{DBus: &priv},
	}}
	got := InstanceTrailing(app, paths, "/usr/bin/psboxa", 3)
	if !containsSeq(got, "--setenv", DBusEnv, config.DBusPrivate) {
		t.Fatalf("private dbus should set %s: %v", DBusEnv, got)
	}
}

func containsSeq(got []string, want ...string) bool {
	if len(want) == 0 || len(got) < len(want) {
		return false
	}
	for i := 0; i+len(want) <= len(got); i++ {
		if slices.Equal(got[i:i+len(want)], want) {
			return true
		}
	}
	return false
}
