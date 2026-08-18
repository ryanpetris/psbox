package systemd

// Session-bus address helpers.

import (
	"testing"
)

func TestSessionBusAddressEnv(t *testing.T) {
	t.Setenv("DBUS_SESSION_BUS_ADDRESS", "unix:path=/tmp/session-bus")
	if got := sessionBusAddress("/run/user/1000"); got != "unix:path=/tmp/session-bus" {
		t.Fatalf("got %q", got)
	}
}

func TestSessionBusAddressFallback(t *testing.T) {
	t.Setenv("DBUS_SESSION_BUS_ADDRESS", "")
	if got := sessionBusAddress("/run/user/1000"); got != "unix:path=/run/user/1000/bus" {
		t.Fatalf("got %q", got)
	}
	if got := sessionBusAddress(""); got != "" {
		t.Fatalf("empty runtime: %q", got)
	}
}

func TestUnixPathAddr(t *testing.T) {
	t.Parallel()

	path, ok := unixPathAddr("unix:path=/run/user/1000/bus")
	if !ok || path != "/run/user/1000/bus" {
		t.Fatalf("plain path: %q %v", path, ok)
	}
	path, ok = unixPathAddr("unix:guid=abc,path=/run/user/1000/bus")
	if !ok || path != "/run/user/1000/bus" {
		t.Fatalf("guid+path: %q %v", path, ok)
	}
	if _, ok := unixPathAddr("unix:abstract=/tmp/dbus-x"); ok {
		t.Fatal("abstract must not look like a filesystem path")
	}
	if _, ok := unixPathAddr("tcp:host=127.0.0.1,port=1234"); ok {
		t.Fatal("tcp")
	}
}
