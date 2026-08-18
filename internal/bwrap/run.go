package bwrap

// Exec composed bwrap argument lists.

import (
	"fmt"
	"os"
	"syscall"
)

// Exec replaces the current process with bwrap. trailing is appended after
// the composed option flags.
func Exec(f Flags, env Env, trailing []string) error {
	if f.Home != "" {
		if err := ensureHomeDir(f.Home); err != nil {
			return err
		}
	}
	argv := Argv(f, env, trailing)
	return syscall.Exec(argv[0], argv, os.Environ())
}

func ensureHomeDir(home string) error {
	info, err := os.Stat(home)
	if err == nil {
		if !info.IsDir() {
			return fmt.Errorf("home path %s is not a directory", home)
		}
		return nil
	}
	if os.IsNotExist(err) {
		if mkErr := os.MkdirAll(home, 0o700); mkErr != nil {
			return fmt.Errorf("create home %s: %w", home, mkErr)
		}
		return nil
	}
	return err
}
