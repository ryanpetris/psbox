package instance

// Daemon request races and configuration reloads use an in-process protocol peer.

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"petris.dev/psbox/internal/config"
)

func TestSandboxPeerProcess(t *testing.T) {
	if os.Getenv("PSBOX_TEST_PEER") != "1" {
		return
	}
	f := os.NewFile(3, "agent")
	conn, err := net.FileConn(f)
	_ = f.Close()
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	peer := conn.(*net.UnixConn)
	for {
		msg, files, err := ReadMsg(peer, 3)
		closeFiles(files)
		if err != nil {
			return
		}
		switch msg.Type {
		case TypeQueryLiveness:
			err = WriteMsg(peer, Message{Type: TypeLiveness, ID: msg.ID, Workload: os.Getenv("PSBOX_TEST_BUSY") == "1"}, nil)
		case TypeSpawn:
			err = WriteMsg(peer, Message{Type: TypeSpawned, ID: msg.ID, PID: os.Getpid()}, nil)
			if os.Getenv("PSBOX_TEST_DISCONNECT") == "1" {
				return
			}
			if err == nil {
				err = WriteMsg(peer, Message{Type: TypeExited, ID: msg.ID, Code: intPtr(0)}, nil)
			}
		}
		if err != nil {
			return
		}
	}
}

func testDaemon(t *testing.T) *Daemon {
	t.Helper()
	root := t.TempDir()
	objects := filepath.Join(root, "objects")
	for _, dir := range []string{objects} {
		if err := os.Mkdir(dir, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	copyFixture(t, "testdata/app.yaml", filepath.Join(objects, "app.yaml"), 0o644)
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("PSBOX_TEST_PEER", "1")
	paths := config.Paths{Home: root, Root: filepath.Join(root, "Sandbox"), ObjectPath: objects, ConfigHome: filepath.Join(root, "config"), RuntimeDir: root, Agent: "/usr/bin/psboxa"}
	log := slog.New(slog.DiscardHandler)
	d := NewDaemon(log, config.NewLoader(log), paths)
	d.command = func(ctx context.Context, _ string, _ ...string) *exec.Cmd {
		return exec.CommandContext(ctx, exe, "-test.run=TestSandboxPeerProcess")
	}
	d.sandbox = "app"
	t.Cleanup(func() {
		if d.active != nil {
			d.active.stop()
		}
	})
	return d
}

func copyFixture(t *testing.T, source, dest string, mode os.FileMode) {
	t.Helper()
	data, err := os.ReadFile(source)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(dest, data, mode); err != nil {
		t.Fatal(err)
	}
}

func TestDaemonImmediateSpawnReplies(t *testing.T) {
	d := testDaemon(t)
	for range 30 {
		server, client := seqpacketPair(t)
		done := make(chan struct{})
		go func() { defer close(done); d.serveClient(t.Context(), server) }()
		if err := WriteMsg(client, Message{Type: TypeSpawn, Sandbox: "app", Argv: []string{"true"}}, nil); err != nil {
			t.Fatal(err)
		}
		if err := client.SetReadDeadline(time.Now().Add(2 * time.Second)); err != nil {
			t.Fatal(err)
		}
		for _, want := range []string{TypeSpawned, TypeExited} {
			msg, _, err := ReadMsg(client, 0)
			if err != nil || msg.Type != want {
				t.Fatalf("got %s, %v; want %s", msg.Type, err, want)
			}
		}
		_ = client.Close()
		<-done
	}
	d.active.mu.Lock()
	defer d.active.mu.Unlock()
	if len(d.active.waiters) != 0 {
		t.Fatalf("leaked waiters: %d", len(d.active.waiters))
	}
}

func TestDaemonAgentLossFinishesClient(t *testing.T) {
	t.Setenv("PSBOX_TEST_DISCONNECT", "1")
	d := testDaemon(t)
	server, client := seqpacketPair(t)
	defer client.Close()
	done := make(chan struct{})
	go func() { defer close(done); d.serveClient(t.Context(), server) }()
	if err := WriteMsg(client, Message{Type: TypeSpawn, Sandbox: "app", Argv: []string{"true"}}, nil); err != nil {
		t.Fatal(err)
	}
	_ = client.SetReadDeadline(time.Now().Add(2 * time.Second))
	first, _, err := ReadMsg(client, 0)
	if err != nil || first.Type != TypeSpawned {
		t.Fatalf("spawn: %+v, %v", first, err)
	}
	reply, _, err := ReadMsg(client, 0)
	if err != nil {
		t.Fatal(err)
	}
	if err := exitedError(reply); err == nil || !strings.Contains(err.Error(), "disconnected") {
		t.Fatalf("agent loss: %+v, %v", reply, err)
	}
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("client stayed blocked")
	}
}

func TestDaemonReloadConfiguration(t *testing.T) {
	for _, busy := range []bool{false, true} {
		t.Run(fmt.Sprint("busy=", busy), func(t *testing.T) {
			t.Setenv("PSBOX_TEST_BUSY", map[bool]string{false: "0", true: "1"}[busy])
			d := testDaemon(t)
			first, err := d.ensureSandbox(t.Context(), Message{})
			if err != nil {
				t.Fatal(err)
			}
			copyFixture(t, "testdata/no-audio.yaml", filepath.Join(d.paths.ObjectPath, "app.yaml"), 0o644)
			second, err := d.ensureSandbox(t.Context(), Message{})
			if err != nil {
				t.Fatal(err)
			}
			if busy {
				if first != second {
					t.Fatal("rebuilt busy sandbox")
				}
			} else {
				if first == second || hashesEqual(first.hashes, second.hashes) {
					t.Fatal("idle sandbox did not rebuild with changed options")
				}
			}
		})
	}
}

func TestDaemonReloadCollectionRootAndRestoreDefault(t *testing.T) {
	d := testDaemon(t)
	first, err := d.ensureSandbox(t.Context(), Message{})
	if err != nil {
		t.Fatal(err)
	}
	override := filepath.Join(t.TempDir(), "homes")
	data, err := os.ReadFile("testdata/options.yaml")
	if err != nil {
		t.Fatal(err)
	}
	optionPath := filepath.Join(d.paths.ObjectPath, "options.yaml")
	if err := os.WriteFile(optionPath, []byte(strings.ReplaceAll(string(data), "/tmp/psbox-test-root", override)), 0o644); err != nil {
		t.Fatal(err)
	}
	second, err := d.ensureSandbox(t.Context(), Message{})
	if err != nil {
		t.Fatal(err)
	}
	if second == first || hashesEqual(first.hashes, second.hashes) {
		t.Fatal("collection root change did not rebuild")
	}
	if _, err := os.Stat(filepath.Join(override, "app")); err != nil {
		t.Fatalf("new private home: %v", err)
	}
	if err := os.Remove(optionPath); err != nil {
		t.Fatal(err)
	}
	third, err := d.ensureSandbox(t.Context(), Message{})
	if err != nil {
		t.Fatal(err)
	}
	if third == second || !hashesEqual(first.hashes, third.hashes) {
		t.Fatal("removing collection root did not restore process default")
	}
}

func TestDaemonRejectsInvalidReload(t *testing.T) {
	d := testDaemon(t)
	first, err := d.ensureSandbox(t.Context(), Message{})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(filepath.Join(d.paths.ObjectPath, "app.yaml")); err != nil {
		t.Fatal(err)
	}
	if _, err := d.ensureSandbox(t.Context(), Message{}); err == nil {
		t.Fatal("missing sandbox accepted")
	}
	if first.finished() {
		t.Fatal("failed reload terminated existing sandbox")
	}
}

func TestStalledHostOpenerDoesNotBlockAgentReplies(t *testing.T) {
	local, peer := seqpacketPair(t)
	defer peer.Close()
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	run := &sandboxProcess{conn: local, cancel: cancel, readerDone: make(chan struct{}), waiters: map[string]chan Message{}}
	started := make(chan struct{}, maxHostURIOpens)
	finished := make(chan struct{}, maxHostURIOpens)
	go run.readAgent(ctx, func(ctx context.Context, uri string) error {
		started <- struct{}{}
		<-ctx.Done()
		finished <- struct{}{}
		return ctx.Err()
	})
	defer func() { _ = local.Close(); <-run.readerDone }()
	for i := range maxHostURIOpens {
		if err := WriteMsg(peer, Message{Type: TypeOpenURI, ID: fmt.Sprint("url-", i), URI: "https://example.com"}, nil); err != nil {
			t.Fatal(err)
		}
		<-started
	}
	if err := WriteMsg(peer, Message{Type: TypeOpenURI, ID: "overflow", URI: "https://example.com"}, nil); err != nil {
		t.Fatal(err)
	}
	_ = peer.SetReadDeadline(time.Now().Add(time.Second))
	rejected, _, err := ReadMsg(peer, 0)
	if err != nil || rejected.ID != "overflow" || rejected.OK {
		t.Fatalf("overflow: %+v, %v", rejected, err)
	}
	id, replies, err := run.request(Message{Type: TypeQueryLiveness}, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer run.unwaiter(id)
	request, _, err := ReadMsg(peer, 0)
	if err != nil {
		t.Fatal(err)
	}
	if err := WriteMsg(peer, Message{Type: TypeLiveness, ID: request.ID, Workload: true}, nil); err != nil {
		t.Fatal(err)
	}
	select {
	case reply := <-replies:
		if !reply.Workload {
			t.Fatalf("wrong reply: %+v", reply)
		}
	case <-time.After(time.Second):
		t.Fatal("opener blocked liveness reply")
	}
	_ = local.Close()
	<-run.readerDone
	if len(finished) != maxHostURIOpens {
		t.Fatalf("unjoined URL workers: %d", len(finished))
	}
}
