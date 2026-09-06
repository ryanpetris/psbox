package objects

// Desktop install and orphan tests.

import (
	"bytes"
	"encoding/json"
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

	svc := NewService(config.NewLoader(slog.New(slog.DiscardHandler)), slog.New(slog.DiscardHandler))
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
	svc := NewService(config.NewLoader(slog.New(slog.DiscardHandler)), slog.New(slog.DiscardHandler))
	if err := svc.Render(paths, "bad", os.Stdout); err == nil {
		t.Fatal("expected sandbox collection error")
	}
}

func TestInstallRenderFailurePreservesLaunchers(t *testing.T) {
	paths := testPaths(t)
	paths.ObjectPath = filepath.Join(t.TempDir(), "objects")
	if err := os.Mkdir(paths.ObjectPath, 0o755); err != nil {
		t.Fatal(err)
	}
	configPath := filepath.Join(paths.ObjectPath, "app.yaml")
	body, err := os.ReadFile("testdata/install/app.yaml")
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(configPath, body, 0o644); err != nil {
		t.Fatal(err)
	}
	var logs bytes.Buffer
	log := slog.New(slog.NewJSONHandler(&logs, nil))
	svc := NewService(config.NewLoader(log), log)
	if err := svc.Install(paths); err != nil {
		t.Fatal(err)
	}
	desktop := filepath.Join(paths.DataHome, "applications", "app.desktop")
	before, err := os.ReadFile(desktop)
	if err != nil {
		t.Fatal(err)
	}
	invalid, err := os.ReadFile("testdata/bad-override.yaml")
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(configPath, invalid, 0o644); err != nil {
		t.Fatal(err)
	}
	if err := svc.Install(paths); err == nil || !strings.Contains(err.Error(), "override pattern") {
		t.Fatalf("expected actual render failure, got %v", err)
	}
	after, err := os.ReadFile(desktop)
	if err != nil || !bytes.Equal(before, after) {
		t.Fatalf("configured launcher changed: %v", err)
	}
	for _, line := range strings.Split(strings.TrimSpace(logs.String()), "\n") {
		if !json.Valid([]byte(line)) {
			t.Fatalf("non-structured progress: %q", line)
		}
	}
}
