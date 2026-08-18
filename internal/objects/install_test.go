package objects

// Desktop install and orphan tests.

import (
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"petris.dev/psbox/internal/config"
)

func TestInstallMarkerAndOrphanRemoval(t *testing.T) {
	t.Parallel()

	root := t.TempDir()
	objects := filepath.Join(root, "objects")
	if err := os.MkdirAll(objects, 0o755); err != nil {
		t.Fatal(err)
	}
	err := os.WriteFile(filepath.Join(objects, "app.yaml"), []byte(`
version: v1
kind: Application
---
version: v1
kind: FreedesktopEntry
spec:
  value: |
    [Desktop Entry]
    Name=App
    Exec=app
`), 0o644)
	if err != nil {
		t.Fatal(err)
	}

	paths := testPaths(t)
	paths.ObjectPath = objects
	paths.DataHome = filepath.Join(root, "data")
	paths.ConfigHome = filepath.Join(root, "config")

	svc := NewService(config.NewLoader(slog.New(slog.DiscardHandler)))
	if err := svc.Install(paths); err != nil {
		t.Fatal(err)
	}
	desktop := filepath.Join(paths.DataHome, "applications", "app.desktop")
	body, err := os.ReadFile(desktop)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(string(body), GeneratorMarker+"\n") {
		t.Fatalf("missing generator marker: %s", body)
	}

	foreign := filepath.Join(paths.DataHome, "applications", "foreign.desktop")
	if err := os.WriteFile(foreign, []byte("# @generator other-tool\n[Desktop Entry]\nName=Other\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	ownedOrphan := filepath.Join(paths.DataHome, "applications", "gone.desktop")
	if err := os.WriteFile(ownedOrphan, []byte(GeneratorMarker+"\n[Desktop Entry]\nName=Gone\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := svc.Install(paths); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(foreign); err != nil {
		t.Fatal("foreign generator must be left alone")
	}
	if _, err := os.Stat(ownedOrphan); !os.IsNotExist(err) {
		t.Fatal("owned orphan must be removed")
	}
}

func TestRenderHardErrorOnSandbox(t *testing.T) {
	t.Parallel()

	root := t.TempDir()
	objects := filepath.Join(root, "objects")
	if err := os.MkdirAll(objects, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(objects, "bad.yaml"), []byte(`
version: v1
kind: Application
metadata:
  name: bad
spec:
  options:
    sandbox: missing
`), 0o644); err != nil {
		t.Fatal(err)
	}
	paths := testPaths(t)
	paths.ObjectPath = objects
	svc := NewService(config.NewLoader(slog.New(slog.DiscardHandler)))
	if err := svc.Render(paths, "bad", os.Stdout); err == nil {
		t.Fatal("expected sandbox collection error")
	}
}
