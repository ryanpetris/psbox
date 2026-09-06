package bwrap

// Printed commands preserve literal arguments when evaluated by a POSIX shell.

import (
	"os/exec"
	"reflect"
	"strings"
	"testing"
)

func TestQuoteShellRoundTrip(t *testing.T) {
	args := []string{"", "plain", "with space", "with\ttab", "with\nnewline", "`printf substituted`", "$(printf substituted)", "single'quote", `double"quote`, `back\slash`, "a=b", "~", "*", "#comment", "café"}
	cmd := exec.CommandContext(t.Context(), "sh", "-c", "printf '%s\\000' "+Quote(args))
	out, err := cmd.Output()
	if err != nil {
		t.Fatal(err)
	}
	got := strings.Split(strings.TrimSuffix(string(out), "\x00"), "\x00")
	if !reflect.DeepEqual(got, args) {
		t.Fatalf("got %#v, want %#v", got, args)
	}
}
