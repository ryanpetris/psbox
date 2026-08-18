package client

// CLI log presentation tests.

import (
	"bytes"
	"log/slog"
	"strings"
	"testing"

	"petris.dev/psbox/internal/config"
)

func TestCLIHandlerMessageOnly(t *testing.T) {
	t.Parallel()

	var buf bytes.Buffer
	log := slog.New(newCLIHandler(&buf, ""))
	log.Warn("skipping object /tmp/x.yaml: yaml: line 5: mapping values are not allowed in this context",
		"file", "/tmp/x.yaml",
		"error", "yaml: line 5: mapping values are not allowed in this context",
	)
	got := buf.String()
	if got != "skipping object /tmp/x.yaml: yaml: line 5: mapping values are not allowed in this context\n" {
		t.Fatalf("got %q", got)
	}
	for _, part := range []string{"time=", "level=", "msg=", "file="} {
		if strings.Contains(got, part) {
			t.Fatalf("structured field %q leaked: %q", part, got)
		}
	}
}

func TestCLIHandlerStructured(t *testing.T) {
	t.Parallel()

	var buf bytes.Buffer
	log := slog.New(newCLIHandler(&buf, config.LogStructured))
	log.Warn("skipping object", "file", "/tmp/x.yaml")
	got := buf.String()
	if !strings.Contains(got, "msg=\"skipping object\"") {
		t.Fatalf("missing msg: %q", got)
	}
	if !strings.Contains(got, "file=/tmp/x.yaml") {
		t.Fatalf("missing attr: %q", got)
	}
	if !strings.Contains(got, "level=WARN") {
		t.Fatalf("missing level: %q", got)
	}
}

func TestCLIHandlerIgnoresUnknownMode(t *testing.T) {
	t.Parallel()

	var buf bytes.Buffer
	log := slog.New(newCLIHandler(&buf, "json"))
	log.Info("hello", "k", "v")
	if buf.String() != "hello\n" {
		t.Fatalf("got %q", buf.String())
	}
}

func TestNewLoggerReadsEnv(t *testing.T) {
	t.Setenv(config.EnvLog, config.LogStructured)
	h := NewLogger().Handler()
	if _, ok := h.(*messageHandler); ok {
		t.Fatal("PSBOX_LOG=structured must not use the message handler")
	}
}
