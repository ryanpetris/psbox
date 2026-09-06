package instance

// Agent shutdown joins children, readers, and the private bus.

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"os/exec"
	"strconv"
	"syscall"
	"testing"
	"time"

	"golang.org/x/sys/unix"

	"petris.dev/psbox/internal/config"
)

func TestAgentMovedProcessGroup(t *testing.T) {
	if os.Getenv("PSBOX_TEST_MOVED_GROUP") == "1" {
		group, err := strconv.Atoi(os.Getenv("PSBOX_TEST_PARENT_GROUP"))
		if err != nil {
			t.Fatal(err)
		}
		if err := syscall.Setpgid(0, group); err != nil {
			t.Fatal(err)
		}
		if _, err := fmt.Fprintln(os.Stdout, "ready"); err != nil {
			t.Fatal(err)
		}
		time.Sleep(30 * time.Second)
		return
	}
	t.Setenv(config.HostURLsEnv, config.HostURLsOff)
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	group, err := syscall.Getpgid(0)
	if err != nil {
		t.Fatal(err)
	}
	local, peer := seqpacketPair(t)
	defer local.Close()
	defer peer.Close()
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	null, err := os.OpenFile(os.DevNull, os.O_RDWR, 0)
	if err != nil {
		t.Fatal(err)
	}
	a := NewAgent(slog.New(slog.DiscardHandler))
	if err := a.spawn(local, Message{ID: "1", Argv: []string{exe, "-test.run=TestAgentMovedProcessGroup"}, Env: map[string]string{"PSBOX_TEST_MOVED_GROUP": "1", "PSBOX_TEST_PARENT_GROUP": strconv.Itoa(group)}}, []*os.File{null, w, null}); err != nil {
		t.Fatal(err)
	}
	spawned, _, err := ReadMsg(peer, 0)
	if err != nil || spawned.Type != TypeSpawned {
		t.Fatalf("spawn: %+v, %v", spawned, err)
	}
	defer a.shutdown()
	_ = r.SetReadDeadline(time.Now().Add(2 * time.Second))
	if line, err := bufio.NewReader(r).ReadString('\n'); err != nil || line != "ready\n" {
		t.Fatalf("child readiness %q: %v", line, err)
	}
	a.shutdown()
	joined := make(chan struct{})
	go func() { a.workers.Wait(); close(joined) }()
	select {
	case <-joined:
	case <-time.After(2 * time.Second):
		t.Fatal("child outside original process group was not reaped")
	}
}

func TestAgentRunShutdown(t *testing.T) {
	for _, privateBus := range []bool{false, true} {
		for _, unexpected := range []bool{false, true} {
			if !privateBus && unexpected {
				continue
			}
			t.Run(fmt.Sprintf("bus=%v/unexpected=%v", privateBus, unexpected), func(t *testing.T) {
				if privateBus {
					if _, err := exec.LookPath("dbus-daemon"); err != nil {
						t.Skip("dbus-daemon unavailable")
					}
				}
				t.Setenv(config.HostURLsEnv, config.HostURLsOff)
				if privateBus {
					t.Setenv(config.DBusEnv, config.DBusPrivate)
				} else {
					t.Setenv(config.DBusEnv, "")
				}
				local, peer := seqpacketPair(t)
				defer local.Close()
				defer peer.Close()
				raw, err := local.SyscallConn()
				if err != nil {
					t.Fatal(err)
				}
				var fd int
				var dupErr error
				if err := raw.Control(func(source uintptr) { fd, dupErr = unix.FcntlInt(source, unix.F_DUPFD_CLOEXEC, 0) }); err != nil {
					t.Fatal(err)
				}
				if dupErr != nil {
					t.Fatal(dupErr)
				}
				t.Setenv(config.ControlFDEnv, strconv.Itoa(fd))
				ctx, cancel := context.WithCancel(t.Context())
				defer cancel()
				a := NewAgent(slog.New(slog.DiscardHandler))
				result := make(chan error, 1)
				go func() { result <- a.Run(ctx) }()
				if err := WriteMsg(peer, Message{Type: TypeSpawn, ID: "1", Argv: []string{"/bin/sleep", "30"}}, nil); err != nil {
					t.Fatal(err)
				}
				_ = peer.SetReadDeadline(time.Now().Add(3 * time.Second))
				spawned, _, err := ReadMsg(peer, 0)
				if err != nil || spawned.Type != TypeSpawned {
					t.Fatalf("spawn: %+v, %v", spawned, err)
				}
				if unexpected {
					a.mu.Lock()
					bus := a.busProc
					a.mu.Unlock()
					if err := bus.Kill(); err != nil {
						t.Fatal(err)
					}
				} else {
					cancel()
				}
				select {
				case err := <-result:
					if err == nil {
						t.Fatal("expected shutdown reason")
					}
					if !unexpected && !errors.Is(err, context.Canceled) {
						t.Fatalf("cancellation: %v", err)
					}
				case <-time.After(3 * time.Second):
					t.Fatal("agent shutdown did not join workers")
				}
				a.mu.Lock()
				defer a.mu.Unlock()
				if a.live != 0 || len(a.spawns) != 0 {
					t.Fatalf("unreaped workloads: %d", a.live)
				}
			})
		}
	}
}
