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
