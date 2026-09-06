package systemd

// A lightweight session bus verifies stop failures against an exported manager.

import (
	"bufio"
	"context"
	"errors"
	"os"
	"os/exec"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/godbus/dbus/v5"
)

type stopHandler struct {
	state      string
	stateError string
	stopError  string
	cancel     context.CancelFunc
	attempts   atomic.Int32
}

func (h *stopHandler) StopUnit(string, string) (dbus.ObjectPath, *dbus.Error) {
	h.attempts.Add(1)
	return "", dbus.NewError(h.stopError, []any{"stop failed"})
}

func (h *stopHandler) GetUnit(string) (dbus.ObjectPath, *dbus.Error) {
	if h.cancel != nil {
		h.cancel()
	}
	if h.stateError != "" {
		return "", dbus.NewError(h.stateError, []any{"state unavailable"})
	}
	return "/unit", nil
}

func (h *stopHandler) Get(string, string) (dbus.Variant, *dbus.Error) {
	return dbus.MakeVariant(h.state), nil
}

func (h *stopHandler) Subscribe() *dbus.Error { return nil }

func testBus(t *testing.T) string {
	t.Helper()
	binary, err := exec.LookPath("dbus-daemon")
	if err != nil {
		t.Skip("dbus-daemon unavailable")
	}
	cmd := exec.CommandContext(t.Context(), binary, "--session", "--nofork", "--print-address=1")
	out, err := cmd.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	cmd.Stderr = os.Stderr
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = cmd.Process.Kill(); _ = cmd.Wait() })
	addr, err := bufio.NewReader(out).ReadString('\n')
	if err != nil {
		t.Fatal(err)
	}
	return strings.TrimSpace(addr)
}

func TestStopRequiresEstablishedTerminalState(t *testing.T) {
	addr := testBus(t)
	t.Setenv("DBUS_SESSION_BUS_ADDRESS", addr)
	server, err := dbus.Connect(addr)
	if err != nil {
		t.Fatal(err)
	}
	defer server.Close()
	if _, err := server.RequestName(dest, dbus.NameFlagDoNotQueue); err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		name, state, stateError, stopError string
		success, cancel                    bool
		attempts                           int32
	}{
		{"inactive", "inactive", "", "org.freedesktop.DBus.Error.Failed", true, false, 1},
		{"failed", "failed", "", "org.freedesktop.DBus.Error.Failed", true, false, 1},
		{"missing", "", "org.freedesktop.systemd1.NoSuchUnit", "org.freedesktop.DBus.Error.Failed", true, false, 1},
		{"active", "active", "", "org.freedesktop.DBus.Error.Failed", false, false, 1},
		{"deactivating", "deactivating", "", "org.freedesktop.DBus.Error.Failed", false, false, 1},
		{"state query failed", "", "org.freedesktop.DBus.Error.Disconnected", "org.freedesktop.DBus.Error.NoReply", false, false, RetryAttempts},
		{"canceled", "", "org.freedesktop.DBus.Error.Disconnected", "org.freedesktop.DBus.Error.NoReply", false, true, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(t.Context(), 3*time.Second)
			defer cancel()
			handler := &stopHandler{state: tc.state, stateError: tc.stateError, stopError: tc.stopError}
			if tc.cancel {
				handler.cancel = cancel
			}
			if err := server.Export(handler, objPath, iface); err != nil {
				t.Fatal(err)
			}
			if err := server.Export(handler, "/unit", "org.freedesktop.DBus.Properties"); err != nil {
				t.Fatal(err)
			}
			user := NewUser()
			defer user.dropConn()
			err := user.Stop(ctx, "app.service")
			if (err == nil) != tc.success {
				t.Fatalf("Stop = %v; success=%v", err, tc.success)
			}
			if tc.cancel && !errors.Is(err, context.Canceled) {
				t.Fatalf("cancel: %v", err)
			}
			if handler.attempts.Load() != tc.attempts {
				t.Fatalf("attempts=%d, want %d", handler.attempts.Load(), tc.attempts)
			}
		})
	}
}
