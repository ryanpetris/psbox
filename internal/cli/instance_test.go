package cli

// Instance command argument tests.

import (
	"io"
	"testing"
)

func TestInstanceStopRequiresName(t *testing.T) {
	t.Parallel()

	cmd := testCommand(t, t.TempDir())
	cmd.SetOut(io.Discard)
	cmd.SetErr(io.Discard)
	cmd.SetArgs([]string{"instance", "stop"})
	if err := cmd.Execute(); err == nil {
		t.Fatal("expected error")
	}

	cmd = testCommand(t, t.TempDir())
	cmd.SetOut(io.Discard)
	cmd.SetErr(io.Discard)
	cmd.SetArgs([]string{"instance", "stop", "--all", "brave"})
	if err := cmd.Execute(); err == nil {
		t.Fatal("expected error")
	}
}
