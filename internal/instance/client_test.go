package instance

// Instance client connect tests.

import (
	"context"
	"strings"
	"sync"
	"testing"

	"petris.dev/psbox/internal/config"
)

type recordStarter struct {
	mu    sync.Mutex
	units []string
}

func (r *recordStarter) Start(_ context.Context, unit string) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.units = append(r.units, unit)
	return nil
}

func TestConnectRetriesSocketAfterConnectError(t *testing.T) {
	t.Parallel()

	runtime := t.TempDir()
	escaped := Escape(Identity("app", "default"))
	path := SocketPath(runtime, escaped)
	sock, svc := UnitNames(escaped)

	st := &recordStarter{}
	c := NewClient(nil, config.Paths{RuntimeDir: runtime}, st)
	_, err := c.connect(t.Context(), path, sock, svc)
	if err == nil {
		t.Fatal("expected connect error")
	}
	st.mu.Lock()
	got := append([]string(nil), st.units...)
	st.mu.Unlock()
	if len(got) != 2 || got[0] != sock || got[1] != sock {
		t.Fatalf("started %v, want socket twice", got)
	}
	for _, unit := range got {
		if strings.HasSuffix(unit, ".service") {
			t.Fatalf("must not start the service unit: %v", got)
		}
	}
}
