package cli

// Launch argv and print tests.

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/spf13/cobra"

	"petris.dev/psbox/internal/config"
	"petris.dev/psbox/internal/instance"
	"petris.dev/psbox/internal/launch"
	"petris.dev/psbox/internal/objects"
	"petris.dev/psbox/internal/sandbox"
	"petris.dev/psbox/internal/systemd"
	"petris.dev/psbox/internal/units"
)

func TestArgvSplit(t *testing.T) {
	t.Parallel()

	objects := writeLaunchObjects(t)
	cases := []struct {
		name string
		args []string
		want []string
	}{
		{
			name: "default command extra positionals",
			args: []string{"launch", "--objects", objects, "--print", "brave", "https://example.com"},
			want: []string{"psbox", "sandbox", "--home", "--", "--bind", "dbus-launch", "brave", "https://example.com"},
		},
		{
			name: "double dash args",
			args: []string{"launch", "--objects", objects, "--print", "brave", "--", "--incognito"},
			want: []string{"psbox", "sandbox", "--", "dbus-launch", "brave", "--incognito"},
		},
		{
			name: "command override",
			args: []string{"launch", "--objects", objects, "--print", "brave", "--command", "--", "env", "FOO=bar", "brave"},
			want: []string{"psbox", "sandbox", "--", "dbus-launch", "env", "FOO=bar", "brave"},
		},
		{
			name: "flags after app",
			args: []string{"launch", "--objects", objects, "brave", "--print"},
			want: []string{"psbox", "sandbox", "--", "dbus-launch", "brave"},
		},
		{
			name: "isolation oneshot prints sandbox",
			args: []string{"launch", "--objects", objects, "--print", "--isolation", "oneshot", "brave"},
			want: []string{"psbox", "sandbox", "--", "dbus-launch", "brave"},
		},
		{
			name: "isolation instance still prints sandbox",
			args: []string{"launch", "--objects", objects, "--print", "--isolation", "instance", "brave"},
			want: []string{"psbox", "sandbox", "--", "dbus-launch", "brave"},
		},
		{
			name: "isolation instance print-bwrap",
			args: []string{"launch", "--objects", objects, "--print-bwrap", "--isolation", "instance", "brave"},
			want: []string{"/usr/bin/bwrap", "dbus-launch", "brave"},
		},
		{
			name: "default-args when no extras",
			args: []string{"launch", "--objects", objects, "--print", "kit"},
			want: []string{"psbox", "sandbox", "--", "kit", "--safe-mode"},
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			out, err := execPrint(t, objects, tc.args)
			if err != nil {
				t.Fatal(err)
			}
			for _, part := range tc.want {
				if !strings.Contains(out, part) {
					t.Fatalf("output %q missing %q", out, part)
				}
			}
			if strings.Contains(out, "psboxa") || strings.Contains(out, "systemctl") {
				t.Fatalf("print leaked instance machinery: %s", out)
			}
		})
	}
}

func TestRootDoesNotLaunchApp(t *testing.T) {
	t.Parallel()

	objects := writeLaunchObjects(t)
	_, err := execPrint(t, objects, []string{"brave"})
	if err == nil {
		t.Fatal("root must not treat an application name as a command")
	}
}

func TestLaunchApplicationNamedLaunch(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	body := `
version: v1
kind: Application
metadata:
  name: launch
spec:
  exec: my-launcher
`
	if err := os.WriteFile(filepath.Join(dir, "launch.yaml"), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	out, err := execPrint(t, dir, []string{"launch", "--objects", dir, "--print", "launch"})
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(out, "my-launcher") {
		t.Fatalf("application named launch should still launch: %s", out)
	}
}

func TestIsolationInstanceMutex(t *testing.T) {
	t.Parallel()

	objects := writeLaunchObjects(t)
	_, err := execPrint(t, objects, []string{"launch", "--objects", objects, "--print", "--isolation", "instance", "--instance", "work", "brave"})
	if err == nil {
		t.Fatal("expected mutex error")
	}
}

func TestPrintBwrap(t *testing.T) {
	t.Parallel()

	objects := writeLaunchObjects(t)
	out, err := execPrint(t, objects, []string{"launch", "--objects", objects, "--print-bwrap", "brave"})
	if err != nil {
		t.Fatal(err)
	}
	if !strings.HasPrefix(out, "/usr/bin/bwrap ") {
		t.Fatalf("expected bwrap line: %s", out)
	}
	if !strings.Contains(out, "dbus-launch") || !strings.Contains(out, "brave") {
		t.Fatalf("expected workload: %s", out)
	}
	if strings.Contains(out, "psboxa") {
		t.Fatal(out)
	}
}

func TestBothPrintFlagsError(t *testing.T) {
	t.Parallel()

	objects := writeLaunchObjects(t)
	_, err := execPrint(t, objects, []string{"launch", "--objects", objects, "--print", "--print-bwrap", "brave"})
	if err == nil {
		t.Fatal("expected error")
	}
}

func TestObjectsPlusInstanceLaunchError(t *testing.T) {
	t.Parallel()

	objects := writeLaunchObjects(t)
	_, err := execPrint(t, objects, []string{"launch", "--objects", objects, "--isolation", "instance", "brave"})
	if err == nil {
		t.Fatal("expected --objects + instance error")
	}
}

func TestEmptyCommandError(t *testing.T) {
	t.Parallel()

	objects := writeLaunchObjects(t)
	_, err := execPrint(t, objects, []string{"launch", "--objects", objects, "--print", "brave", "--command"})
	if err == nil {
		t.Fatal("expected empty --command error")
	}
}

func execPrint(t *testing.T, objectPath string, args []string) (string, error) {
	t.Helper()
	cmd := testCommand(t, objectPath)
	var buf bytes.Buffer
	cmd.SetOut(&buf)
	cmd.SetErr(io.Discard)
	cmd.SetArgs(args)
	err := cmd.Execute()
	return buf.String(), err
}

func testCommand(t *testing.T, objectPath string) *cobra.Command {
	t.Helper()
	log := slog.New(slog.DiscardHandler)
	paths := config.Paths{
		Home:           t.TempDir(),
		ConfigHome:     t.TempDir(),
		DataHome:       t.TempDir(),
		RuntimeDir:     t.TempDir(),
		ObjectPath:     objectPath,
		Root:           filepath.Join(t.TempDir(), "Sandbox"),
		WaylandDisplay: config.DefaultWaylandDisplay,
		PipewireCore:   config.DefaultPipewireCore,
	}
	loader := config.NewLoader(log)
	return NewRoot(launch.NewService(loader, paths, instance.NewClient(log, paths, nilStarter{})), objects.NewService(loader, log), sandbox.NewService(paths), units.NewService(nilUser{}), paths)
}

func writeLaunchObjects(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	body := `
version: v1
kind: Application
metadata:
  name: brave
spec:
  exec: brave
  options:
    dbus: private
  bwrap-args:
    - [--bind, ~/Downloads]
`
	if err := os.WriteFile(filepath.Join(dir, "brave.yaml"), []byte(body), 0o644); err != nil {
		t.Fatal(err)
	}
	kit := `
version: v1
kind: Application
metadata:
  name: kit
spec:
  exec: kit
  default-args:
    - --safe-mode
`
	if err := os.WriteFile(filepath.Join(dir, "kit.yaml"), []byte(kit), 0o644); err != nil {
		t.Fatal(err)
	}
	return dir
}

type nilStarter struct{}

func (nilStarter) Start(context.Context, string) error { return nil }

type nilUser struct{}

func (nilUser) Start(context.Context, string) error { return nil }

func (nilUser) Stop(context.Context, string) error { return nil }

func (nilUser) List(context.Context, []string) ([]systemd.Unit, error) { return nil, nil }

type failedOutput struct{}

var _ io.Writer = failedOutput{}

func (failedOutput) Write([]byte) (int, error) { return 0, io.ErrClosedPipe }

func TestPrintReportsOutputFailure(t *testing.T) {
	objects := writeLaunchObjects(t)
	for _, args := range [][]string{
		{"launch", "--print", "brave"},
		{"launch", "--print-bwrap", "brave"},
		{"sandbox", "--home-tmpfs", "--print", "--", "true"},
	} {
		cmd := testCommand(t, objects)
		cmd.SetOut(failedOutput{})
		cmd.SetErr(io.Discard)
		cmd.SetArgs(args)
		if err := cmd.ExecuteContext(t.Context()); !errors.Is(err, io.ErrClosedPipe) {
			t.Fatalf("%v: %v", args, err)
		}
	}
}
