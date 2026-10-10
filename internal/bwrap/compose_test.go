package bwrap

// Compose flag tests.

import (
	"slices"
	"testing"

	"petris.dev/psbox/internal/config"
)

func TestComposeDev(t *testing.T) {
	t.Parallel()

	env := Env{Home: "/home/u", RuntimeDir: "/run/user/1000"}
	for _, mode := range []string{"", config.DevPrivate, config.DevHost, config.DevPrivateSHM} {
		t.Run(mode, func(t *testing.T) {
			got := Compose(Flags{Dev: mode}, env)
			host := mode == config.DevHost || mode == config.DevPrivateSHM
			if containsSeq(got, "--dev-bind", "/dev", "/dev") != host || containsSeq(got, "--dev", "/dev") == host {
				t.Fatalf("device root for %q: %v", mode, got)
			}
			if mode == config.DevPrivateSHM {
				if !containsSeq(got, "--dev-bind", "/dev", "/dev", "--perms", "1777", "--tmpfs", "/dev/shm") {
					t.Fatalf("private shared memory must cover host /dev/shm with mode 1777: %v", got)
				}
			} else if containsSeq(got, "--tmpfs", "/dev/shm") {
				t.Fatalf("unexpected shared memory override: %v", got)
			}
			for _, path := range []string{"/dev/snd", "/dev/v4l", "/dev/input", "/dev/dri"} {
				if containsSeq(got, "--dev-bind-try", path, path) == host {
					t.Fatalf("selective device bind %q for %q: %v", path, mode, got)
				}
			}
			if !containsSeq(got, "--ro-bind-try", "/run/user/1000/pulse", "/run/user/1000/pulse") || !containsSeq(got, "--ro-bind-try", "/run/user/1000/wayland-0", "/run/user/1000/wayland-0") {
				t.Fatalf("device mode must preserve audio and display integration: %v", got)
			}
		})
	}
}

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

func TestComposeNewSession(t *testing.T) {
	t.Parallel()

	env := Env{Home: "/home/u", RuntimeDir: "/run/user/1000"}
	got := Compose(Flags{NoHome: true, NoAudio: true, NoVideo: true, NoDBus: true, NoFontconfig: true, NoUdev: true, NoEvdev: true, NoDisplay: true}, env)
	if !slices.Contains(got, "--new-session") {
		t.Fatalf("missing --new-session: %v", got)
	}
	if !containsSeq(got, "--die-with-parent", "--unshare-pid", "--new-session") {
		t.Fatalf("isolation flags should be together: %v", got)
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
