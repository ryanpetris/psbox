package cli

// Generated completion tests.

import (
	"bytes"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
)

func TestGeneratedCompletionsParse(t *testing.T) {
	t.Parallel()

	cmd := testCommand(t, t.TempDir())
	dir := t.TempDir()

	var bash bytes.Buffer
	if err := cmd.GenBashCompletionV2(&bash, true); err != nil {
		t.Fatal(err)
	}
	bashPath := filepath.Join(dir, "psbox.bash")
	if err := os.WriteFile(bashPath, bash.Bytes(), 0o644); err != nil {
		t.Fatal(err)
	}
	if out, err := exec.Command("bash", "-n", bashPath).CombinedOutput(); err != nil {
		t.Fatalf("bash -n: %v\n%s", err, out)
	}

	var zsh bytes.Buffer
	if err := cmd.GenZshCompletion(&zsh); err != nil {
		t.Fatal(err)
	}
	zshPath := filepath.Join(dir, "_psbox")
	if err := os.WriteFile(zshPath, zsh.Bytes(), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := exec.LookPath("zsh"); err != nil {
		t.Log("zsh not installed; skipped zsh -n")
		return
	}
	if out, err := exec.Command("zsh", "-n", zshPath).CombinedOutput(); err != nil {
		t.Fatalf("zsh -n: %v\n%s", err, out)
	}
}

func TestCompletionCommandExistsForPsboxOnly(t *testing.T) {
	t.Parallel()

	cmd := testCommand(t, t.TempDir())
	found := false
	for _, c := range cmd.Commands() {
		if c.Name() == "completion" {
			found = true
		}
	}
	if !found {
		t.Fatal("expected cobra completion command on psbox")
	}
}
