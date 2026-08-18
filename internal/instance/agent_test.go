package instance

// Agent spawn log tests.

import (
	"bytes"
	"log/slog"
	"os"
	"strings"
	"testing"
	"time"
)

func TestSpawnLogsCommandStartAndExit(t *testing.T) {
	truePath := lookupTrue()
	if truePath == "" {
		t.Skip("no /bin/true or /usr/bin/true")
	}
	t.Setenv(HostURLsEnv, HostURLsOff)

	var buf bytes.Buffer
	a := NewAgent(slog.New(slog.NewTextHandler(&buf, nil)))
	agentConn, clientConn := seqpacketPair(t)
	t.Cleanup(func() {
		_ = agentConn.Close()
		_ = clientConn.Close()
	})

	null, err := os.OpenFile(os.DevNull, os.O_RDWR, 0)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = null.Close() })

	if err := a.spawn(agentConn, Message{
		Type: TypeSpawn,
		ID:   "1",
		Argv: []string{truePath, "--help-is-not-used"},
	}, []*os.File{null, null, null}); err != nil {
		t.Fatal(err)
	}

	started, _, err := ReadMsg(clientConn, 0)
	if err != nil {
		t.Fatal(err)
	}
	if started.Type != TypeSpawned {
		t.Fatalf("first reply %q", started.Type)
	}

	_ = clientConn.SetReadDeadline(time.Now().Add(2 * time.Second))
	exited, _, err := ReadMsg(clientConn, 0)
	if err != nil {
		t.Fatal(err)
	}
	if exited.Type != TypeExited {
		t.Fatalf("second reply %q", exited.Type)
	}

	got := buf.String()
	if !strings.Contains(got, `msg="command started"`) || !strings.Contains(got, "command="+truePath) {
		t.Fatalf("missing start log: %s", got)
	}
	if !strings.Contains(got, `msg="command exited"`) || !strings.Contains(got, "command="+truePath) {
		t.Fatalf("missing exit log: %s", got)
	}
	if strings.Contains(got, "--help-is-not-used") {
		t.Fatalf("must not log later argv: %s", got)
	}
}

func lookupTrue() string {
	for _, path := range []string{"/bin/true", "/usr/bin/true"} {
		if st, err := os.Stat(path); err == nil && !st.IsDir() {
			return path
		}
	}
	return ""
}
