package bwrap

// Compose flag tests.

import (
	"slices"
	"testing"
)

func TestComposeCacheTmpfs(t *testing.T) {
	t.Parallel()

	env := Env{Home: "/home/u", RuntimeDir: "/run/user/1000"}
	got := Compose(Flags{Home: "/home/u/Sandbox/app", NoAudio: true, NoVideo: true, NoDBus: true, NoFontconfig: true, NoUdev: true, NoEvdev: true, NoDisplay: true}, env)
	if !containsSeq(got, "--tmpfs", "/home/u/.cache") {
		t.Fatalf("default cache should be tmpfs: %v", got)
	}

	persist := Compose(Flags{Home: "/home/u/Sandbox/app", CacheHome: true, NoAudio: true, NoVideo: true, NoDBus: true, NoFontconfig: true, NoUdev: true, NoEvdev: true, NoDisplay: true}, env)
	if containsSeq(persist, "--tmpfs", "/home/u/.cache") {
		t.Fatalf("cache: home must not mount ~/.cache tmpfs: %v", persist)
	}

	ephemeralHome := Compose(Flags{HomeTmpfs: true, NoAudio: true, NoVideo: true, NoDBus: true, NoFontconfig: true, NoUdev: true, NoEvdev: true, NoDisplay: true}, env)
	if containsSeq(ephemeralHome, "--tmpfs", "/home/u/.cache") {
		t.Fatalf("tmpfs home does not need a cache mount: %v", ephemeralHome)
	}
}

func TestComposeDownloads(t *testing.T) {
	t.Parallel()

	env := Env{Home: "/home/u", RuntimeDir: "/run/user/1000"}
	got := Compose(Flags{Home: "/home/u/Sandbox/app", Downloads: true, NoAudio: true, NoVideo: true, NoDBus: true, NoFontconfig: true, NoUdev: true, NoEvdev: true, NoDisplay: true}, env)
	if !containsSeq(got, "--bind", "/home/u/Downloads", "/home/u/Downloads") {
		t.Fatalf("downloads bind: %v", got)
	}

	off := Compose(Flags{Home: "/home/u/Sandbox/app", NoAudio: true, NoVideo: true, NoDBus: true, NoFontconfig: true, NoUdev: true, NoEvdev: true, NoDisplay: true}, env)
	if containsSeq(off, "--bind", "/home/u/Downloads", "/home/u/Downloads") {
		t.Fatalf("downloads default off: %v", off)
	}
}

func TestComposeDBusBinds(t *testing.T) {
	t.Parallel()

	env := Env{Home: "/home/u", RuntimeDir: "/run/user/1000"}
	none := Compose(Flags{NoHome: true, NoDBus: true, NoAudio: true, NoVideo: true, NoFontconfig: true, NoUdev: true, NoEvdev: true, NoDisplay: true}, env)
	if slices.Contains(none, "/run/dbus") || slices.Contains(none, "/run/user/1000/bus") {
		t.Fatalf("--no-dbus should bind no dbus sockets: %v", none)
	}

	sessionOff := Compose(Flags{NoHome: true, NoSessionDBus: true, NoAudio: true, NoVideo: true, NoFontconfig: true, NoUdev: true, NoEvdev: true, NoDisplay: true}, env)
	if !slices.Contains(sessionOff, "/run/dbus") {
		t.Fatalf("--no-session-dbus should still bind the system bus: %v", sessionOff)
	}
	if slices.Contains(sessionOff, "/run/user/1000/bus") {
		t.Fatalf("--no-session-dbus must not bind the host session bus: %v", sessionOff)
	}
}
