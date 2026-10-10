package bwrap

// Sandbox argv and flag tests.

import (
	"slices"
	"testing"

	"petris.dev/psbox/internal/config"
)

func TestApplicationDevFlags(t *testing.T) {
	t.Parallel()

	for _, mode := range []string{config.DevPrivate, config.DevHost, config.DevPrivateSHM} {
		t.Run(mode, func(t *testing.T) {
			app := &config.Application{Spec: config.ApplicationSpec{Options: config.ApplicationOptions{Dev: &mode}}}
			flags := FlagsFromApplication(app, testPaths(t))
			if flags.Dev != mode {
				t.Fatalf("Dev = %q, want %q", flags.Dev, mode)
			}
			args := flags.Args()
			if mode != config.DevPrivate && !containsSeq(args, "--dev", mode) {
				t.Fatalf("missing device mode in sandbox arguments: %v", args)
			}
			if mode == config.DevPrivate && slices.Contains(args, "--dev") {
				t.Fatalf("default mode should be omitted: %v", args)
			}
		})
	}
}

func TestLaunchSandboxArgvPrintShape(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	priv := config.DBusPrivate
	app := &config.Application{
		Name: "brave",
		Spec: config.ApplicationSpec{
			Exec: []string{"brave"},
			Options: config.ApplicationOptions{
				DBus: &priv,
			},
			BwrapArgs: [][]string{{"--bind", "~/Downloads"}, {"--tmpfs", "~/.cache"}},
		},
	}
	eff := &config.Effective{
		Name:        "brave",
		SandboxName: "brave",
		Target:      app,
		Exec:        []string{"brave"},
	}
	argv := LaunchSandboxArgv(eff, []string{"https://example.com"}, paths)
	if argv[0] != "--home" {
		t.Fatalf("expected --home, got %v", argv)
	}
	if !slices.Contains(argv, "--no-dbus") {
		t.Fatalf("private dbus should pass --no-dbus: %v", argv)
	}
	if slices.Contains(argv, "--no-session-dbus") {
		t.Fatalf("private dbus should not use --no-session-dbus: %v", argv)
	}
	sep := slices.Index(argv, "--")
	if sep < 0 {
		t.Fatalf("missing --: %v", argv)
	}
	trail := argv[sep+1:]
	if !slices.Contains(trail, "dbus-launch") {
		t.Fatalf("expected dbus-launch: %v", trail)
	}
	if !slices.Contains(trail, "brave") {
		t.Fatalf("expected workload: %v", trail)
	}
	if !slices.Contains(trail, "https://example.com") {
		t.Fatalf("expected extra args: %v", trail)
	}
	if slices.Contains(trail, "psboxa") {
		t.Fatalf("printed command must not include psboxa: %v", trail)
	}
	if !slices.Contains(trail, "--bind") {
		t.Fatalf("expected yaml bwrap-args: %v", trail)
	}
}

func TestFlagsArgsCacheHome(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	home := config.CacheHome
	app := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Options: config.ApplicationOptions{Cache: &home},
	}}
	got := FlagsFromApplication(app, paths).Args()
	if !slices.Contains(got, "--cache") || !slices.Contains(got, config.CacheHome) {
		t.Fatalf("cache: home should print --cache home: %v", got)
	}

	def := FlagsFromApplication(&config.Application{Name: "app"}, paths).Args()
	if slices.Contains(def, "--cache") {
		t.Fatalf("default cache must not emit --cache: %v", def)
	}
}

func TestFlagsArgsDownloads(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	on := true
	app := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Options: config.ApplicationOptions{Downloads: &on},
	}}
	got := FlagsFromApplication(app, paths).Args()
	if !slices.Contains(got, "--downloads") {
		t.Fatalf("downloads should print --downloads: %v", got)
	}
}

func TestExpandBindShorthand(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	got := ExpandBwrapArgs([][]string{{"--bind", "~/Downloads"}}, paths)
	if len(got) != 3 {
		t.Fatalf("bind shorthand: %v", got)
	}
	if got[1] != got[2] {
		t.Fatalf("dest should copy src: %v", got)
	}
}
