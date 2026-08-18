package config

// Path and token tests.

import (
	"path/filepath"
	"testing"
)

func TestRuntimeDirDefault(t *testing.T) {
	t.Setenv("XDG_RUNTIME_DIR", "")
	got := RuntimeDir()
	want := DefaultRuntimeDir()
	if got != want {
		t.Fatalf("RuntimeDir()=%q, want %q", got, want)
	}
	if filepath.Base(filepath.Dir(got)) != "user" {
		t.Fatalf("expected /run/user/<uid>, got %q", got)
	}
}

func TestRuntimeDirUsesEnv(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("XDG_RUNTIME_DIR", dir)
	if got := RuntimeDir(); got != dir {
		t.Fatalf("RuntimeDir()=%q, want %q", got, dir)
	}
}
