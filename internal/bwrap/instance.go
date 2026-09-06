package bwrap

// Instance-mode bwrap trailing command (the in-sandbox agent).

import (
	"path/filepath"
	"strconv"
	"strings"

	"petris.dev/psbox/internal/config"
)

const xdgOpenName = "xdg-open"

// XDGOpenBinDir is $XDG_RUNTIME_DIR/psbox/bin, prepended to PATH in the sandbox.
func XDGOpenBinDir(runtimeDir string) string {
	if runtimeDir == "" {
		return ""
	}
	return filepath.Join(runtimeDir, "psbox", "bin")
}

// InstanceTrailing is the bwrap command for psboxa, including YAML bwrap-args.
func InstanceTrailing(app *config.Application, paths config.Paths, agent string, controlFD int) []string {
	var cmd []string
	cmd = append(cmd, ExpandBwrapArgs(app.Spec.BwrapArgs, paths)...)
	if !underUsr(agent) {
		cmd = append(cmd, "--ro-bind", agent, agent)
	}
	// A psboxa symlink first on PATH makes `xdg-open` the host-URL helper.
	// /usr/bin/xdg-open stays the real helper for non-forwarded schemes.
	if app.HostURLs() {
		if dir := XDGOpenBinDir(paths.RuntimeDir); dir != "" {
			cmd = append(cmd, "--dir", filepath.Dir(dir))
			cmd = append(cmd, "--dir", dir)
			cmd = append(cmd, "--symlink", agent, filepath.Join(dir, xdgOpenName))
		}
	} else {
		cmd = append(cmd, "--setenv", config.HostURLsEnv, config.HostURLsOff)
	}
	cmd = append(cmd, "--setenv", config.ControlFDEnv, strconv.Itoa(controlFD))
	if app.DBus() == config.DBusPrivate {
		cmd = append(cmd, "--setenv", config.DBusEnv, config.DBusPrivate)
	}
	cmd = append(cmd, agent)
	return cmd
}

func underUsr(path string) bool {
	return path == "/usr" || strings.HasPrefix(path, "/usr/")
}
