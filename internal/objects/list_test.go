package objects

// Object list tests.

import (
	"bytes"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"petris.dev/psbox/internal/config"
)

func TestListApplicationsOmitsSandboxErrors(t *testing.T) {
	t.Parallel()

	root := t.TempDir()
	objects := filepath.Join(root, "objects")
	if err := os.MkdirAll(objects, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(objects, "ok.yaml"), []byte(`
version: v1
kind: Application
metadata:
  name: ok
---
version: v1
kind: FreedesktopEntry
`), 0o644); err != nil {
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
	var buf bytes.Buffer
	if err := svc.List(paths, "applications", true, &buf); err != nil {
		t.Fatal(err)
	}
	out := buf.String()
	if !strings.Contains(out, "ok") {
		t.Fatalf("missing ok: %s", out)
	}
	if strings.Contains(out, "bad") {
		t.Fatalf("listed sandbox error app: %s", out)
	}

	buf.Reset()
	if err := svc.List(paths, "desktop", false, &buf); err != nil {
		t.Fatal(err)
	}
	desktop := buf.String()
	if !strings.Contains(desktop, "APPLICATION") || !strings.Contains(desktop, "ENTRY") {
		t.Fatalf("desktop headers: %s", desktop)
	}
	if !strings.Contains(desktop, "ok") {
		t.Fatalf("desktop list: %s", desktop)
	}
}
