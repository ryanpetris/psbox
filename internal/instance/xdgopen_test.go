package instance

// xdg-open helper tests.

import (
	"bufio"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"golang.org/x/sys/unix"

	"petris.dev/psbox/internal/config"
)

func TestInvokedAsXDGOpen(t *testing.T) {
	t.Parallel()

	yes := [][]string{
		{"xdg-open", "https://example.com"},
		{"/usr/bin/xdg-open", "https://example.com"},
		{"/run/user/1000/psbox/bin/xdg-open", "https://example.com"},
		{"psboxa", "xdg-open", "https://example.com"},
		{"/usr/bin/psboxa", "xdg-open"},
	}
	for _, args := range yes {
		if !InvokedAsXDGOpen(args) {
			t.Errorf("InvokedAsXDGOpen(%q)=false", args)
		}
	}
	no := [][]string{nil, {}, {"psboxa"}, {"/usr/bin/psboxa"}, {"xdg-open.real"}, {"psboxa", "spawn"}}
	for _, args := range no {
		if InvokedAsXDGOpen(args) {
			t.Errorf("InvokedAsXDGOpen(%q)=true", args)
		}
	}
}

func TestXDGOpenArgs(t *testing.T) {
	t.Parallel()

	cases := []struct {
		args []string
		want []string
	}{
		{[]string{"xdg-open", "https://example.com"}, []string{"https://example.com"}},
		{[]string{"psboxa", "xdg-open", "https://example.com"}, []string{"https://example.com"}},
		{[]string{"/usr/bin/psboxa", "xdg-open", "--", "https://example.com"}, []string{"--", "https://example.com"}},
		{[]string{"xdg-open"}, nil},
		{[]string{"psboxa", "xdg-open"}, nil},
	}
	for _, tc := range cases {
		got := xdgOpenArgs(tc.args)
		if !stringSlicesEqual(got, tc.want) {
			t.Errorf("xdgOpenArgs(%q)=%q, want %q", tc.args, got, tc.want)
		}
	}
}

func TestXDGOpenTarget(t *testing.T) {
	t.Parallel()

	cases := []struct {
		args []string
		want string
	}{
		{[]string{"https://example.com"}, "https://example.com"},
		{[]string{"--", "https://example.com"}, "https://example.com"},
		{[]string{"-u", "https://example.com"}, "https://example.com"},
		{[]string{"file:///tmp/x"}, "file:///tmp/x"},
		{[]string{"--help"}, ""},
		{[]string{}, ""},
	}
	for _, tc := range cases {
		if got := xdgOpenTarget(tc.args); got != tc.want {
			t.Errorf("xdgOpenTarget(%q)=%q, want %q", tc.args, got, tc.want)
		}
	}
}

func stringSlicesEqual(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func TestHandleXDGOpenLocal(t *testing.T) {
	t.Parallel()

	a := NewAgent(nil)
	client, server := net.Pipe()
	defer client.Close()
	_ = client.SetDeadline(time.Now().Add(time.Second))
	go a.handleXDGOpen(t.Context(), nil, server, new(atomic.Uint64))
	if _, err := fmt.Fprintln(client, "file:///tmp/x"); err != nil {
		t.Fatal(err)
	}
	line, err := bufio.NewReader(client).ReadString('\n')
	if err != nil {
		t.Fatal(err)
	}
	if got := strings.TrimSpace(line); got != "local" {
		t.Fatalf("reply %q, want local", got)
	}
}

func TestHandleXDGOpenForwardsHTTP(t *testing.T) {
	t.Parallel()

	agentConn, daemonConn := seqpacketPair(t)
	defer agentConn.Close()
	defer daemonConn.Close()

	go func() {
		msg, _, err := ReadMsg(daemonConn, 0)
		if err != nil {
			return
		}
		if msg.Type != TypeOpenURI || msg.URI != "https://example.com" {
			_ = WriteMsg(daemonConn, Message{Type: TypeOpenURIResult, ID: msg.ID, OK: false, Message: "unexpected"}, nil)
			return
		}
		_ = WriteMsg(daemonConn, Message{Type: TypeOpenURIResult, ID: msg.ID, OK: true}, nil)
	}()

	a := NewAgent(nil)
	go a.readControlForTest(t, agentConn)

	client, server := net.Pipe()
	defer client.Close()
	_ = client.SetDeadline(time.Now().Add(time.Second))
	go a.handleXDGOpen(t.Context(), agentConn, server, new(atomic.Uint64))
	if _, err := fmt.Fprintln(client, "https://example.com"); err != nil {
		t.Fatal(err)
	}
	line, err := bufio.NewReader(client).ReadString('\n')
	if err != nil {
		t.Fatal(err)
	}
	if got := strings.TrimSpace(line); got != "ok" {
		t.Fatalf("reply %q, want ok", got)
	}
}

func TestHandleXDGOpenForwardError(t *testing.T) {
	t.Parallel()

	agentConn, daemonConn := seqpacketPair(t)
	defer agentConn.Close()
	defer daemonConn.Close()

	go func() {
		msg, _, err := ReadMsg(daemonConn, 0)
		if err != nil {
			return
		}
		_ = WriteMsg(daemonConn, Message{Type: TypeOpenURIResult, ID: msg.ID, OK: false, Message: "refused open_uri scheme"}, nil)
	}()

	a := NewAgent(nil)
	go a.readControlForTest(t, agentConn)

	client, server := net.Pipe()
	defer client.Close()
	_ = client.SetDeadline(time.Now().Add(time.Second))
	go a.handleXDGOpen(t.Context(), agentConn, server, new(atomic.Uint64))
	if _, err := fmt.Fprintln(client, "https://example.com"); err != nil {
		t.Fatal(err)
	}
	line, err := bufio.NewReader(client).ReadString('\n')
	if err != nil {
		t.Fatal(err)
	}
	if got := strings.TrimSpace(line); got != "error refused open_uri scheme" {
		t.Fatalf("reply %q", got)
	}
}

func TestRequestHostOpen(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("XDG_RUNTIME_DIR", dir)

	agentConn, daemonConn := seqpacketPair(t)
	defer agentConn.Close()
	defer daemonConn.Close()

	go func() {
		msg, _, err := ReadMsg(daemonConn, 0)
		if err != nil {
			return
		}
		_ = WriteMsg(daemonConn, Message{Type: TypeOpenURIResult, ID: msg.ID, OK: true}, nil)
	}()

	a := NewAgent(nil)
	ctx := t.Context()
	go a.serveXDGOpen(ctx, agentConn)
	go a.readControlForTest(t, agentConn)
	waitForSocket(t, xdgOpenSocketPath())

	if err := requestHostOpen("https://example.com"); err != nil {
		t.Fatal(err)
	}
}

func TestRequestHostOpenMissingSocket(t *testing.T) {
	t.Setenv("XDG_RUNTIME_DIR", t.TempDir())
	if err := requestHostOpen("https://example.com"); err == nil {
		t.Fatal("expected error without a listener")
	}
}

func TestSpawnEnvInheritsRuntimeDir(t *testing.T) {
	t.Setenv("XDG_RUNTIME_DIR", "/run/user/1000")
	t.Setenv("WAYLAND_DISPLAY", "wayland-1")
	env := spawnEnv(map[string]string{
		"PATH":            "/usr/bin",
		"XDG_RUNTIME_DIR": "/somewhere/else",
		"WAYLAND_DISPLAY": "wayland-0",
	}, "")
	if env["XDG_RUNTIME_DIR"] != "/run/user/1000" {
		t.Fatalf("XDG_RUNTIME_DIR=%q", env["XDG_RUNTIME_DIR"])
	}
	if env["WAYLAND_DISPLAY"] != "wayland-1" {
		t.Fatalf("WAYLAND_DISPLAY=%q", env["WAYLAND_DISPLAY"])
	}
	wantPATH := "/run/user/1000/psbox/bin:/usr/bin"
	if env["PATH"] != wantPATH {
		t.Fatalf("PATH=%q, want %q", env["PATH"], wantPATH)
	}
}

func TestSpawnEnvPATHWithoutHostURLs(t *testing.T) {
	t.Setenv("XDG_RUNTIME_DIR", "/run/user/1000")
	t.Setenv(config.HostURLsEnv, config.HostURLsOff)
	env := spawnEnv(map[string]string{"PATH": "/usr/bin"}, "")
	if env["PATH"] != "/usr/bin" {
		t.Fatalf("PATH=%q", env["PATH"])
	}
}

func TestSpawnEnvPATHWithoutClientPATH(t *testing.T) {
	t.Setenv("XDG_RUNTIME_DIR", "/run/user/1000")
	env := spawnEnv(map[string]string{}, "")
	want := "/run/user/1000/psbox/bin:" + config.DefaultPATH
	if env["PATH"] != want {
		t.Fatalf("PATH=%q, want %q", env["PATH"], want)
	}
}

func TestSpawnEnvRuntimeDirDefault(t *testing.T) {
	t.Setenv("XDG_RUNTIME_DIR", "")
	env := spawnEnv(map[string]string{}, "")
	if env["XDG_RUNTIME_DIR"] != config.DefaultRuntimeDir() {
		t.Fatalf("XDG_RUNTIME_DIR=%q", env["XDG_RUNTIME_DIR"])
	}
}

func (a *Agent) readControlForTest(t *testing.T, conn *net.UnixConn) {
	t.Helper()
	for {
		msg, files, err := ReadMsg(conn, 0)
		if err != nil {
			return
		}
		if err := a.handle(t.Context(), conn, msg, files); err != nil {
			return
		}
	}
}

func seqpacketPair(t *testing.T) (*net.UnixConn, *net.UnixConn) {
	t.Helper()
	fds, err := unix.Socketpair(unix.AF_UNIX, unix.SOCK_SEQPACKET|unix.SOCK_CLOEXEC, 0)
	if err != nil {
		t.Fatal(err)
	}
	left := unixConnFromFD(t, fds[0], "left")
	right := unixConnFromFD(t, fds[1], "right")
	return left, right
}

func unixConnFromFD(t *testing.T, fd int, name string) *net.UnixConn {
	t.Helper()
	file := os.NewFile(uintptr(fd), name)
	if file == nil {
		t.Fatalf("NewFile %s", name)
	}
	conn, err := net.FileConn(file)
	_ = file.Close()
	if err != nil {
		t.Fatal(err)
	}
	uc, ok := conn.(*net.UnixConn)
	if !ok {
		_ = conn.Close()
		t.Fatalf("%s is not a unix socket", name)
	}
	return uc
}

func waitForSocket(t *testing.T, path string) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for {
		if _, err := os.Stat(path); err == nil {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("socket %s not created", path)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func TestXDGOpenSocketPathUsesRuntimeDir(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("XDG_RUNTIME_DIR", dir)
	got := xdgOpenSocketPath()
	if filepath.Dir(got) != dir {
		t.Fatalf("path %q not under %q", got, dir)
	}
	if filepath.Base(got) != xdgOpenSockName {
		t.Fatalf("path %q", got)
	}
}
