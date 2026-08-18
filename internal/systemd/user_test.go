package systemd

// User D-Bus client tests.

import (
	"testing"

	"github.com/godbus/dbus/v5"
)

func TestVariantInts(t *testing.T) {
	t.Parallel()

	if got := variantUint32(dbus.MakeVariant(uint32(12))); got != 12 {
		t.Fatalf("u32: %d", got)
	}
	if got := variantUint32(dbus.MakeVariant(uint64(9))); got != 9 {
		t.Fatalf("u32 from u64: %d", got)
	}
	if got := variantUint64(dbus.MakeVariant(uint64(1_700_000_000_000_000))); got != 1_700_000_000_000_000 {
		t.Fatalf("u64: %d", got)
	}
	if variantUint32(dbus.MakeVariant("x")) != 0 || variantUint64(dbus.MakeVariant(true)) != 0 {
		t.Fatal("wrong types must be zero")
	}
}

func TestParseJobRemoved(t *testing.T) {
	t.Parallel()

	want := dbus.ObjectPath("/org/freedesktop/systemd1/job/3")
	matched, result, ok := parseJobRemoved([]any{uint32(3), want, "psboxd@x.socket", "done"}, want)
	if !ok || !matched || result != "done" {
		t.Fatalf("got matched=%v result=%q ok=%v", matched, result, ok)
	}

	matched, result, ok = parseJobRemoved([]any{uint32(3), want, "psboxd@x.socket", "failed"}, want)
	if !ok || !matched || result != "failed" {
		t.Fatalf("failed job: matched=%v result=%q ok=%v", matched, result, ok)
	}

	other := dbus.ObjectPath("/org/freedesktop/systemd1/job/9")
	matched, _, ok = parseJobRemoved([]any{uint32(9), other, "u", "done"}, want)
	if !ok || matched {
		t.Fatal("other job must not match")
	}

	if _, _, ok := parseJobRemoved([]any{"bad"}, want); ok {
		t.Fatal("short body")
	}
}
