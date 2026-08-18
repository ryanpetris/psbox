package config

// Object YAML load tests.

import (
	"bytes"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestLoadPathNameAndSandbox(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	writeYAML(t, filepath.Join(dir, "firefox.yaml"), `
version: v1
kind: Application
spec:
  options:
    dbus: private
  bwrap-args:
    - [--bind, ~/Downloads]
---
version: v1
kind: FreedesktopEntry
`)
	writeYAML(t, filepath.Join(dir, "firefox-work.yaml"), `
version: v1
kind: Application
spec:
  exec: firefox
  options:
    sandbox: firefox
`)
	writeYAML(t, filepath.Join(dir, "mixed.yaml"), `
version: v1
kind: Application
metadata:
  name: good
spec:
  exec: /bin/true
---
version: v1
kind: NotAKind
metadata:
  name: skipped
`)

	var buf bytes.Buffer
	loader := NewLoader(slog.New(slog.NewTextHandler(&buf, nil)))
	col, err := loader.LoadPath(dir, testPaths(dir))
	if err != nil {
		t.Fatal(err)
	}

	if _, ok := col.Applications["firefox"]; !ok {
		t.Fatal("expected firefox from filename stem")
	}
	if col.FreedesktopEntries["firefox"] == nil || col.FreedesktopEntries["firefox"].Application != "firefox" {
		t.Fatalf("desktop fixup: %+v", col.FreedesktopEntries["firefox"])
	}
	if _, ok := col.Applications["firefox-work"]; !ok {
		t.Fatal("expected firefox-work")
	}
	if _, ok := col.Applications["good"]; !ok {
		t.Fatal("good object in mixed file should load")
	}
	if _, ok := col.Applications["skipped"]; ok {
		t.Fatal("invalid object should be skipped")
	}
	if !bytes.Contains(buf.Bytes(), []byte("skipping object")) {
		t.Fatalf("expected skip warning, log=%s", buf.String())
	}

	names := col.ListApplications()
	if !contains(names, "firefox-work") {
		t.Fatalf("valid referrer should list: %v", names)
	}
}

func TestLoadPathMalformedYAMLStops(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	writeYAML(t, filepath.Join(dir, "broken.yaml"), `
version: v1
kind: Application
metadata:
  name: broken
spec:
  options:
    dbus: private
      extra: true
`)
	writeYAML(t, filepath.Join(dir, "good.yaml"), `
version: v1
kind: Application
metadata:
  name: good
spec:
  exec: /bin/true
`)
	writeYAML(t, filepath.Join(dir, "mixed.yaml"), `
version: v1
kind: Application
metadata:
  name: kept
spec:
  exec: /bin/true
---
version: v1
kind: Application
metadata:
  name: after
spec:
  options:
    dbus: private
      extra: true
---
version: v1
kind: Application
metadata:
  name: unreachable
spec:
  exec: /bin/false
`)

	var buf bytes.Buffer
	loader := NewLoader(slog.New(slog.NewTextHandler(&buf, nil)))

	done := make(chan struct{})
	var col *Collection
	var err error
	go func() {
		defer close(done)
		col, err = loader.LoadPath(dir, testPaths(dir))
	}()
	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("LoadPath hung on malformed YAML")
	}
	if err != nil {
		t.Fatal(err)
	}
	if _, ok := col.Applications["good"]; !ok {
		t.Fatal("sibling file must still load")
	}
	if _, ok := col.Applications["kept"]; !ok {
		t.Fatal("documents before a syntax error must load")
	}
	if _, ok := col.Applications["broken"]; ok {
		t.Fatal("malformed file must not produce an application")
	}
	if _, ok := col.Applications["after"]; ok {
		t.Fatal("document with a syntax error must be skipped")
	}
	if _, ok := col.Applications["unreachable"]; ok {
		t.Fatal("documents after a syntax error must not be read")
	}

	warns := 0
	for _, line := range strings.Split(buf.String(), "\n") {
		if strings.Contains(line, "skipping object") {
			warns++
		}
	}
	if warns != 2 {
		t.Fatalf("want one warning per broken file, got %d\n%s", warns, buf.String())
	}
}

func TestSandboxCollectionErrors(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	writeYAML(t, filepath.Join(dir, "base.yaml"), `
version: v1
kind: Application
metadata:
  name: base
`)
	writeYAML(t, filepath.Join(dir, "mid.yaml"), `
version: v1
kind: Application
metadata:
  name: mid
spec:
  options:
    sandbox: base
`)
	writeYAML(t, filepath.Join(dir, "hop.yaml"), `
version: v1
kind: Application
metadata:
  name: hop
spec:
  options:
    sandbox: mid
`)
	writeYAML(t, filepath.Join(dir, "extra.yaml"), `
version: v1
kind: Application
metadata:
  name: extra
spec:
  options:
    sandbox: base
    audio: false
    host-urls: false
    cache: home
  bwrap-args:
    - --unshare-net
`)
	writeYAML(t, filepath.Join(dir, "missing.yaml"), `
version: v1
kind: Application
metadata:
  name: missing
spec:
  options:
    sandbox: nope
`)

	col, err := NewLoader(slog.Default()).LoadPath(dir, testPaths(dir))
	if err != nil {
		t.Fatal(err)
	}

	if _, err := col.RequireApplication("hop"); err == nil {
		t.Fatal("two-hop should be a hard error")
	}
	if _, err := col.RequireApplication("extra"); err == nil {
		t.Fatal("sandbox plus other keys should be a hard error")
	}
	if _, err := col.RequireApplication("missing"); err == nil {
		t.Fatal("dangling sandbox should be a hard error")
	}
	for _, name := range []string{"hop", "extra", "missing"} {
		if contains(col.ListApplications(), name) {
			t.Errorf("%s should be omitted from list", name)
		}
	}
	if _, err := col.RequireApplication("base"); err != nil {
		t.Fatal(err)
	}
}

func TestResolveUsesTargetIdentity(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	writeYAML(t, filepath.Join(dir, "firefox.yaml"), `
version: v1
kind: Application
spec:
  options:
    isolation: instance
    dbus: private
`)
	writeYAML(t, filepath.Join(dir, "firefox-work.yaml"), `
version: v1
kind: Application
spec:
  exec: firefox
  default-args: ["--profile", "work"]
  env:
    FOO: bar
  options:
    sandbox: firefox
`)

	col, err := NewLoader(slog.Default()).LoadPath(dir, testPaths(dir))
	if err != nil {
		t.Fatal(err)
	}
	eff, err := col.Resolve("firefox-work", Isolation{}, false)
	if err != nil {
		t.Fatal(err)
	}
	if eff.SandboxName != "firefox" || !eff.Isolation.Instance() {
		t.Fatalf("effective = %+v", eff)
	}
	if eff.Env["FOO"] != "bar" || eff.DefaultArgs[1] != "work" {
		t.Fatalf("referrer fields: %+v", eff)
	}
	override, _, err := IsolationFromFlags("", "other")
	if err != nil {
		t.Fatal(err)
	}
	eff, err = col.Resolve("firefox-work", override, true)
	if err != nil {
		t.Fatal(err)
	}
	if eff.Isolation.Name != "other" || eff.SandboxName != "firefox" {
		t.Fatalf("override identity = %+v", eff)
	}
}

func TestCacheOption(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	writeYAML(t, filepath.Join(dir, "home.yaml"), `
version: v1
kind: Application
metadata:
  name: persist
spec:
  options:
    cache: home
`)
	writeYAML(t, filepath.Join(dir, "tmpfs.yaml"), `
version: v1
kind: Application
metadata:
  name: ephemeral
spec:
  options:
    cache: tmpfs
`)
	writeYAML(t, filepath.Join(dir, "default.yaml"), `
version: v1
kind: Application
metadata:
  name: omitted
`)
	writeYAML(t, filepath.Join(dir, "bad.yaml"), `
version: v1
kind: Application
metadata:
  name: bad
spec:
  options:
    cache: true
`)

	var buf bytes.Buffer
	col, err := NewLoader(slog.New(slog.NewTextHandler(&buf, nil))).LoadPath(dir, testPaths(dir))
	if err != nil {
		t.Fatal(err)
	}
	if col.Applications["persist"].Cache() != CacheHome {
		t.Fatal("cache: home")
	}
	if col.Applications["ephemeral"].Cache() != CacheTmpfs {
		t.Fatal("cache: tmpfs")
	}
	if col.Applications["omitted"].Cache() != CacheTmpfs {
		t.Fatal("omitted cache must default to tmpfs")
	}
	if _, ok := col.Applications["bad"]; ok {
		t.Fatal("boolean cache must be rejected")
	}
	if !bytes.Contains(buf.Bytes(), []byte("invalid options.cache")) {
		t.Fatalf("expected cache error, log=%s", buf.String())
	}
}

func TestBrowserTypeDefaults(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	writeYAML(t, filepath.Join(dir, "brave.yaml"), `
version: v1
kind: Application
metadata:
  type: browser
`)
	writeYAML(t, filepath.Join(dir, "firefox.yaml"), `
version: v1
kind: Application
metadata:
  type: browser
spec:
  options:
    dbus: false
`)
	writeYAML(t, filepath.Join(dir, "unknown.yaml"), `
version: v1
kind: Application
metadata:
  name: nope
  type: toaster
`)

	var buf bytes.Buffer
	col, err := NewLoader(slog.New(slog.NewTextHandler(&buf, nil))).LoadPath(dir, testPaths(dir))
	if err != nil {
		t.Fatal(err)
	}
	brave := col.Applications["brave"]
	if brave == nil || brave.Type != TypeBrowser {
		t.Fatalf("brave: %+v", brave)
	}
	if brave.DBus() != DBusPrivate || brave.HostURLs() || !brave.Downloads() {
		t.Fatalf("browser defaults: dbus=%s host-urls=%v downloads=%v", brave.DBus(), brave.HostURLs(), brave.Downloads())
	}
	if col.Applications["firefox"].DBus() != DBusOff {
		t.Fatal("explicit dbus must win over the type")
	}
	if _, ok := col.Applications["nope"]; ok {
		t.Fatal("unknown type must be skipped")
	}
	if !bytes.Contains(buf.Bytes(), []byte("unknown application type")) {
		t.Fatalf("expected type error, log=%s", buf.String())
	}
}

func TestTypeOnReferrerIsError(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	writeYAML(t, filepath.Join(dir, "base.yaml"), `
version: v1
kind: Application
metadata:
  name: base
`)
	writeYAML(t, filepath.Join(dir, "ref.yaml"), `
version: v1
kind: Application
metadata:
  name: ref
  type: browser
spec:
  options:
    sandbox: base
`)
	col, err := NewLoader(slog.Default()).LoadPath(dir, testPaths(dir))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := col.RequireApplication("ref"); err == nil {
		t.Fatal("referrer type should be a collection error")
	}
}

func TestHostURLsOption(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	writeYAML(t, filepath.Join(dir, "off.yaml"), `
version: v1
kind: Application
metadata:
  name: off
spec:
  options:
    host-urls: false
`)
	writeYAML(t, filepath.Join(dir, "on.yaml"), `
version: v1
kind: Application
metadata:
  name: on
`)

	col, err := NewLoader(slog.Default()).LoadPath(dir, testPaths(dir))
	if err != nil {
		t.Fatal(err)
	}
	if col.Applications["off"].HostURLs() {
		t.Fatal("host-urls: false must disable forwarding")
	}
	if !col.Applications["on"].HostURLs() {
		t.Fatal("omitted host-urls must default to true")
	}
}

func writeYAML(t *testing.T, path, body string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
}

func testPaths(root string) Paths {
	return Paths{
		Home:           filepath.Join(root, "home"),
		ConfigHome:     filepath.Join(root, "home", ".config"),
		DataHome:       filepath.Join(root, "home", ".local", "share"),
		RuntimeDir:     filepath.Join(root, "run"),
		ObjectPath:     filepath.Join(root, "objects"),
		Root:           filepath.Join(root, "Sandbox"),
		WaylandDisplay: DefaultWaylandDisplay,
		PipewireCore:   DefaultPipewireCore,
	}
}

func contains(list []string, want string) bool {
	for _, item := range list {
		if item == want {
			return true
		}
	}
	return false
}
