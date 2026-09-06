package instance

// Agent spawn log tests.

import (
	"bytes"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"petris.dev/psbox/internal/config"
)

func TestSpawnLogsCommandStartAndExit(t *testing.T) {
	truePath := lookupTrue()
	if truePath == "" {
		t.Skip("no /bin/true or /usr/bin/true")
	}
	t.Setenv(config.HostURLsEnv, config.HostURLsOff)

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

	a.workers.Wait()
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

func TestExecutableLookupUsesWorkloadPATHAndCwd(t *testing.T) {
	root := t.TempDir()
	first := filepath.Join(root, "first")
	second := filepath.Join(root, "second")
	for _, dir := range []string{first, second} {
		if err := os.Mkdir(dir, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	for path, mode := range map[string]os.FileMode{filepath.Join(root, "app"): 0o755, filepath.Join(first, "app"): 0o644, filepath.Join(second, "app"): 0o755} {
		if err := os.WriteFile(path, nil, mode); err != nil {
			t.Fatal(err)
		}
	}
	for _, tc := range []struct {
		name, command, path, want string
		errno                     error
	}{
		{"no cwd fallback", "app", first + "/missing", "", syscall.ENOENT},
		{"empty PATH", "app", "", "", syscall.ENOENT},
		{"skip non-executable", "app", first + ":" + second, filepath.Join(second, "app"), nil},
		{"permission denied", "app", first, "", syscall.EACCES},
		{"relative PATH", "app", "second", filepath.Join(second, "app"), nil},
		{"explicit relative executable", "./app", first, filepath.Join(root, "app"), nil},
		{"explicit current directory in PATH", "app", ":" + first, filepath.Join(root, "app"), nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, err := lookPath(tc.command, map[string]string{"PATH": tc.path}, root)
			if got != tc.want || !errors.Is(err, tc.errno) {
				t.Fatalf("got %q, %v; want %q, %v", got, err, tc.want, tc.errno)
			}
		})
	}
}

func TestSpawnFailedAcknowledgmentReapsChild(t *testing.T) {
	t.Setenv(config.HostURLsEnv, config.HostURLsOff)
	a := NewAgent(slog.New(slog.DiscardHandler))
	local, peer := seqpacketPair(t)
	defer local.Close()
	if err := peer.Close(); err != nil {
		t.Fatal(err)
	}
	if err := a.spawn(local, Message{ID: "1", Argv: []string{"/bin/sleep", "30"}}, nil); err == nil {
		t.Fatal("expected failed acknowledgment")
	}
	a.workers.Wait()
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.live != 0 || len(a.spawns) != 0 {
		t.Fatalf("unreaped spawn: live=%d, sessions=%d", a.live, len(a.spawns))
	}
}

func TestErrnoOfWrappedProcessError(t *testing.T) {
	for _, errno := range []syscall.Errno{syscall.ENOENT, syscall.EACCES} {
		err := fmt.Errorf("spawn: %w", &os.PathError{Op: "fork/exec", Path: "app", Err: errno})
		if got := errnoOf(err); got != int(errno) {
			t.Fatalf("got %d, want %d", got, errno)
		}
	}
}
