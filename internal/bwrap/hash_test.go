package bwrap

// Config and running hash tests.

import (
	"bytes"
	"os"
	"path/filepath"
	"testing"

	"petris.dev/psbox/internal/config"
)

func TestHashOmitsDefaultsAndSortsKeys(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	env := EnvFromPaths(paths)

	omitted := &config.Application{Name: "app", Spec: config.ApplicationSpec{}}
	explicitDefault := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Options: config.ApplicationOptions{
			Audio:      boolPtr(true),
			Video:      boolPtr(true),
			DBus:       strPtr(config.DBusHost),
			Display:    boolPtr(true),
			Fontconfig: boolPtr(true),
		},
	}}
	a, err := ComputeHashes(omitted, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	b, err := ComputeHashes(explicitDefault, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(a.Config, b.Config) {
		t.Fatal("explicit default values must not change the config hash")
	}

	reordered := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Options: config.ApplicationOptions{
			Fontconfig: boolPtr(false),
			Audio:      boolPtr(false),
		},
	}}
	sameOrder := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Options: config.ApplicationOptions{
			Audio:      boolPtr(false),
			Fontconfig: boolPtr(false),
		},
	}}
	c, err := ComputeHashes(reordered, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	d, err := ComputeHashes(sameOrder, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(c.Config, d.Config) {
		t.Fatal("option key order must not change the config hash")
	}
}

func TestHashBwrapArgsOrderAndHomePin(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	env := EnvFromPaths(paths)

	first := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		BwrapArgs: [][]string{{"--bind", "~/Downloads"}, {"--tmpfs", "/tmp/a"}},
	}}
	second := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		BwrapArgs: [][]string{{"--tmpfs", "/tmp/a"}, {"--bind", "~/Downloads"}},
	}}
	empty := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		BwrapArgs: [][]string{},
	}}
	omitted := &config.Application{Name: "app"}

	h1, err := ComputeHashes(first, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	h2, err := ComputeHashes(second, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Equal(h1.Config, h2.Config) {
		t.Fatal("bwrap-args order is material")
	}

	he, err := ComputeHashes(empty, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	ho, err := ComputeHashes(omitted, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(he.Config, ho.Config) {
		t.Fatal("empty bwrap-args must equal omitted")
	}

	implicit := &config.Application{Name: "app"}
	pinned := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Options: config.ApplicationOptions{Home: strPtr(filepath.Join(paths.Root, "app"))},
	}}
	hi, err := ComputeHashes(implicit, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	hp, err := ComputeHashes(pinned, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Equal(hi.Config, hp.Config) {
		t.Fatal("explicit default home path is a pin")
	}
}

func TestHashBrowserTypeMatchesExplicit(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "typed.yaml"), []byte(`
version: v1
kind: Application
metadata:
  name: typed
  type: browser
`), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "explicit.yaml"), []byte(`
version: v1
kind: Application
metadata:
  name: explicit
spec:
  options:
    dbus: private
    host-urls: false
    downloads: true
`), 0o644); err != nil {
		t.Fatal(err)
	}
	paths := testPaths(t)
	paths.ObjectPath = dir
	col, err := config.NewLoader(nil).LoadPath(dir, paths)
	if err != nil {
		t.Fatal(err)
	}
	env := EnvFromPaths(paths)
	a, err := ComputeHashes(col.Applications["typed"], paths, env)
	if err != nil {
		t.Fatal(err)
	}
	b, err := ComputeHashes(col.Applications["explicit"], paths, env)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(a.Config, b.Config) {
		t.Fatal("type defaults and explicit matching options must hash the same")
	}
}

func TestHashCacheHome(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	env := EnvFromPaths(paths)
	omitted := &config.Application{Name: "app"}
	explicitTmpfs := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Options: config.ApplicationOptions{Cache: strPtr(config.CacheTmpfs)},
	}}
	home := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Options: config.ApplicationOptions{Cache: strPtr(config.CacheHome)},
	}}

	a, err := ComputeHashes(omitted, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	b, err := ComputeHashes(explicitTmpfs, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(a.Config, b.Config) {
		t.Fatal("cache: tmpfs must not change the config hash")
	}
	c, err := ComputeHashes(home, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Equal(a.Config, c.Config) {
		t.Fatal("cache: home must change the config hash")
	}
}

func TestHashHostURLsFalse(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	env := EnvFromPaths(paths)
	omitted := &config.Application{Name: "app"}
	explicitOn := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Options: config.ApplicationOptions{HostURLs: boolPtr(true)},
	}}
	off := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Options: config.ApplicationOptions{HostURLs: boolPtr(false)},
	}}

	a, err := ComputeHashes(omitted, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	b, err := ComputeHashes(explicitOn, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(a.Config, b.Config) {
		t.Fatal("host-urls: true must not change the config hash")
	}
	c, err := ComputeHashes(off, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Equal(a.Config, c.Config) {
		t.Fatal("host-urls: false must change the config hash")
	}
}

func TestHashIgnoresIsolationExecAndEnv(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	env := EnvFromPaths(paths)
	base := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Options: config.ApplicationOptions{DBus: strPtr(config.DBusPrivate)},
	}}
	other := &config.Application{Name: "app", Spec: config.ApplicationSpec{
		Exec:        []string{"/usr/bin/other"},
		DefaultArgs: []string{"--foo"},
		Env:         map[string]string{"FOO": "bar"},
		Options: config.ApplicationOptions{
			DBus:      strPtr(config.DBusPrivate),
			Isolation: strPtr("instance"),
		},
	}}
	a, err := ComputeHashes(base, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	b, err := ComputeHashes(other, paths, env)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(a.Config, b.Config) {
		t.Fatal("isolation, exec, default-args, and env must not enter the config hash")
	}
}

func TestRunningHashOmitsDefaultComposeEnv(t *testing.T) {
	t.Parallel()

	paths := testPaths(t)
	app := &config.Application{Name: "app"}
	def, err := ComputeHashes(app, paths, EnvFromPaths(paths))
	if err != nil {
		t.Fatal(err)
	}
	changed := EnvFromPaths(paths)
	changed.WaylandDisplay = "wayland-1"
	other, err := ComputeHashes(app, paths, changed)
	if err != nil {
		t.Fatal(err)
	}
	if bytes.Equal(def.Running, other.Running) {
		t.Fatal("non-default WAYLAND_DISPLAY must change the running hash")
	}
	if !bytes.Equal(def.Config, other.Config) {
		t.Fatal("compose-env must not change the config hash")
	}
}

func testPaths(t *testing.T) config.Paths {
	t.Helper()
	home := t.TempDir()
	return config.Paths{
		Home:           home,
		ConfigHome:     filepath.Join(home, ".config"),
		DataHome:       filepath.Join(home, ".local", "share"),
		RuntimeDir:     filepath.Join(home, "run"),
		ObjectPath:     filepath.Join(home, ".config", "psbox", "objects"),
		Root:           filepath.Join(home, "Sandbox"),
		WaylandDisplay: config.DefaultWaylandDisplay,
		PipewireCore:   config.DefaultPipewireCore,
	}
}

func boolPtr(v bool) *bool    { return &v }
func strPtr(v string) *string { return &v }
