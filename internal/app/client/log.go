package client

// CLI slog presentation: message-only by default.

import (
	"context"
	"io"
	"log/slog"
	"os"
	"strings"
	"sync"

	"petris.dev/psbox/internal/config"
)

// NewLogger returns the CLI logger. The default handler writes only
// the message. PSBOX_LOG=structured selects slog text.
func NewLogger() *slog.Logger {
	return slog.New(newCLIHandler(os.Stderr, os.Getenv(config.EnvLog)))
}

func newCLIHandler(w io.Writer, mode string) slog.Handler {
	opts := &slog.HandlerOptions{Level: slog.LevelInfo}
	if strings.EqualFold(strings.TrimSpace(mode), config.LogStructured) {
		return slog.NewTextHandler(w, opts)
	}
	return &messageHandler{w: w, min: slog.LevelInfo}
}

type messageHandler struct {
	w   io.Writer
	min slog.Level
	mu  sync.Mutex
}

func (h *messageHandler) Enabled(_ context.Context, level slog.Level) bool {
	return level >= h.min
}

func (h *messageHandler) Handle(_ context.Context, r slog.Record) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	_, err := io.WriteString(h.w, r.Message+"\n")
	return err
}

func (h *messageHandler) WithAttrs([]slog.Attr) slog.Handler { return h }

func (h *messageHandler) WithGroup(string) slog.Handler { return h }
