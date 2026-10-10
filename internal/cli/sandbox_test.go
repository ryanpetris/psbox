package cli

// Sandbox device mode parsing, validation, and completion tests.

import (
	"bytes"
	"slices"
	"strings"
	"testing"

	"github.com/spf13/cobra"
)

func TestSandboxDev(t *testing.T) {
	t.Parallel()

	for _, mode := range []string{"", "private", "true", "host", "private-shm"} {
		t.Run(mode, func(t *testing.T) {
			args := []string{"sandbox", "--no-home", "--print"}
			if mode != "" {
				args = append(args, "--dev", mode)
			}
			args = append(args, "--", "true")
			out, err := execPrint(t, t.TempDir(), args)
			if err != nil {
				t.Fatal(err)
			}
			host := mode != "" && mode != "private"
			if strings.Contains(out, "--dev-bind /dev /dev") != host {
				t.Fatalf("unexpected device root: %s", out)
			}
			if mode == "private-shm" && !strings.Contains(out, "--perms 1777 --tmpfs /dev/shm") {
				t.Fatalf("missing private shared memory: %s", out)
			}
		})
	}
}

func TestSandboxDevValidation(t *testing.T) {
	t.Parallel()

	for _, mode := range []string{"", "false", "off", "unknown", "host,private-shm"} {
		out, err := execPrint(t, t.TempDir(), []string{"sandbox", "--no-home", "--print", "--dev", mode, "--", "true"})
		if err == nil || !strings.Contains(err.Error(), "--dev") || out != "" {
			t.Fatalf("expected invalid dev mode error for %q, output=%q error=%v", mode, out, err)
		}
	}
	for _, mode := range []string{"true", "host", "private-shm"} {
		wantMode := mode
		if mode == "true" {
			wantMode = "host"
		}

		for _, flag := range []string{"--no-audio", "--no-video", "--no-display", "--no-evdev"} {
			args := []string{"sandbox", "--no-home", "--print", "--dev", mode, flag, "--", "true"}
			out, err := execPrint(t, t.TempDir(), args)
			if err == nil || !strings.Contains(err.Error(), "--dev "+wantMode) || !strings.Contains(err.Error(), flag) || out != "" {
				t.Fatalf("expected device exclusion conflict for %v, output=%q error=%v", args, out, err)
			}
		}
	}
	if _, err := execPrint(t, t.TempDir(), []string{"sandbox", "--no-home", "--print", "--dev", "private", "--no-evdev", "--", "true"}); err != nil {
		t.Fatalf("private devices must allow exclusions: %v", err)
	}
}

func TestSandboxDevCompletion(t *testing.T) {
	t.Parallel()

	root := testCommand(t, t.TempDir())
	cmd, _, err := root.Find([]string{"sandbox"})
	if err != nil {
		t.Fatal(err)
	}
	completion, ok := cmd.GetFlagCompletionFunc("dev")
	if !ok {
		t.Fatal("missing dev completion")
	}
	got, directive := completion(cmd, nil, "")
	if !slices.Equal(got, []string{"true", "host", "private", "private-shm"}) || directive != cobra.ShellCompDirectiveNoFileComp {
		t.Fatalf("completion = %v, directive = %v", got, directive)
	}
}

func TestLaunchDev(t *testing.T) {
	t.Parallel()

	for _, mode := range []string{"true", "host", "private-shm"} {
		t.Run(mode, func(t *testing.T) {
			args := []string{"launch", "--print-bwrap", mode}
			out, err := execPrint(t, "testdata/dev", args)
			if err != nil {
				t.Fatal(err)
			}
			if !strings.Contains(out, "--dev-bind /dev /dev") {
				t.Fatalf("missing host devices: %s", out)
			}
			if mode == "private-shm" && !strings.Contains(out, "--perms 1777 --tmpfs /dev/shm") {
				t.Fatalf("missing private shared memory: %s", out)
			}
			printed, err := execPrint(t, "testdata/dev", []string{"launch", "--print", mode})
			if err != nil {
				t.Fatal(err)
			}
			want := mode
			if mode == "true" {
				want = "host"
			}
			if !bytes.Contains([]byte(printed), []byte("--dev "+want)) {
				t.Fatalf("missing normalized device mode: %s", printed)
			}
		})
	}
}
