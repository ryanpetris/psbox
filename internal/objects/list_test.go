package objects

// Object list tests.

import (
	"bytes"
	"errors"
	"io"
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
	svc := NewService(config.NewLoader(slog.New(slog.DiscardHandler)), slog.New(slog.DiscardHandler))
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

type failedWriter struct{}

var _ io.Writer = failedWriter{}

func (failedWriter) Write([]byte) (int, error) { return 0, io.ErrClosedPipe }

func TestRequiredOutputFailures(t *testing.T) {
	paths := testPaths(t)
	paths.ObjectPath = "testdata/install"
	log := slog.New(slog.DiscardHandler)
	svc := NewService(config.NewLoader(log), log)
	for _, kind := range []string{"applications", "desktop", "autostart"} {
		for _, quiet := range []bool{false, true} {
			if kind == "autostart" && quiet {
				continue
			}
			if err := svc.List(paths, kind, quiet, failedWriter{}); !errors.Is(err, io.ErrClosedPipe) {
				t.Errorf("%s quiet=%v: %v", kind, quiet, err)
			}
		}
	}
	if err := svc.Render(paths, "app", failedWriter{}); !errors.Is(err, io.ErrClosedPipe) {
		t.Fatalf("render: %v", err)
	}
}
