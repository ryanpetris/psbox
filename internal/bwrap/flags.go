package bwrap

// Sandbox option flags derived from an application spec.

import (
	"regexp"
	"sort"

	"petris.dev/psbox/internal/config"
)

var bindFlag = regexp.MustCompile(`^--((dev|ro)-)?bind(-try)?$`)

// Flags are the options-side arguments of `psbox sandbox`.
type Flags struct {
	Home          string
	HomeTmpfs     bool
	NoHome        bool
	NoAudio       bool
	NoVideo       bool
	NoDBus        bool
	NoSessionDBus bool
	NoFontconfig  bool
	NoUdev        bool
	NoEvdev       bool
	NoDisplay     bool
	CacheHome     bool
	Downloads     bool
}

// Env is the host environment used to compose bwrap binds and --setenv.
type Env struct {
	Home           string
	RuntimeDir     string
	WaylandDisplay string
	Xauthority     string
	PipewireCore   string
	Root           string
	ObjectPath     string
}

// EnvFromPaths copies display and path fields used at compose time.
func EnvFromPaths(p config.Paths) Env {
	return Env{
		Home:           p.Home,
		RuntimeDir:     p.RuntimeDir,
		WaylandDisplay: p.WaylandDisplay,
		Xauthority:     p.Xauthority,
		PipewireCore:   p.PipewireCore,
		Root:           p.Root,
		ObjectPath:     p.ObjectPath,
	}
}

// FlagsFromApplication builds sandbox flags from the target application.
func FlagsFromApplication(app *config.Application, paths config.Paths) Flags {
	opts := app.Spec.Options
	home := opts.HomeValue()
	var f Flags
	switch home {
	case config.HomeNone:
		f.NoHome = true
	case config.HomeTmpfs:
		f.HomeTmpfs = true
	default:
		f.Home = paths.ExpandHome(app.Name, home)
	}
	if !opts.BoolOption(opts.Audio) {
		f.NoAudio = true
	}
	if !opts.BoolOption(opts.Video) {
		f.NoVideo = true
	}
	switch app.DBus() {
	case config.DBusOff, config.DBusPrivate:
		f.NoDBus = true
	}
	if !opts.BoolOption(opts.Display) {
		f.NoDisplay = true
	}
	if !opts.BoolOption(opts.Fontconfig) {
		f.NoFontconfig = true
	}
	if app.Cache() == config.CacheHome {
		f.CacheHome = true
	}
	if app.Downloads() {
		f.Downloads = true
	}
	return f
}

// Args returns the `psbox sandbox` option flags, without a command separator.
func (f Flags) Args() []string {
	var args []string
	switch {
	case f.NoHome:
		args = append(args, "--no-home")
	case f.HomeTmpfs:
		args = append(args, "--home-tmpfs")
	case f.Home != "":
		args = append(args, "--home", f.Home)
	}
	if f.NoAudio {
		args = append(args, "--no-audio")
	}
	if f.NoVideo {
		args = append(args, "--no-video")
	}
	if f.NoDBus {
		args = append(args, "--no-dbus")
	}
	if f.NoSessionDBus {
		args = append(args, "--no-session-dbus")
	}
	if f.NoFontconfig {
		args = append(args, "--no-fontconfig")
	}
	if f.NoUdev {
		args = append(args, "--no-udev")
	}
	if f.NoEvdev {
		args = append(args, "--no-evdev")
	}
	if f.NoDisplay {
		args = append(args, "--no-display")
	}
	if f.CacheHome {
		args = append(args, "--cache", config.CacheHome)
	}
	if f.Downloads {
		args = append(args, "--downloads")
	}
	return args
}

// ExpandBwrapArgs token-replaces YAML bwrap-args and fills in a bind destination.
func ExpandBwrapArgs(args [][]string, paths config.Paths) []string {
	var out []string
	for _, arg := range args {
		expanded := make([]string, len(arg))
		for i, part := range arg {
			expanded[i] = paths.ReplaceTokens(part)
		}
		if len(expanded) == 2 && bindFlag.MatchString(expanded[0]) {
			expanded = append(expanded, expanded[1])
		}
		out = append(out, expanded...)
	}
	return out
}

// Workload returns exec plus either extra arguments or default-args.
func Workload(exec, extra, defaultArgs []string, paths config.Paths) []string {
	cmd := make([]string, 0, len(exec)+len(extra)+len(defaultArgs))
	for _, part := range exec {
		cmd = append(cmd, paths.ReplaceTokens(part))
	}
	if len(extra) > 0 {
		cmd = append(cmd, extra...)
	} else {
		cmd = append(cmd, defaultArgs...)
	}
	return cmd
}

// TrailingCommand is the tokens after `psbox sandbox … --`.
func TrailingCommand(eff *config.Effective, extra []string, paths config.Paths) []string {
	var cmd []string
	cmd = append(cmd, ExpandBwrapArgs(eff.Target.Spec.BwrapArgs, paths)...)
	cmd = append(cmd, envSetenv(eff.Env)...)
	if eff.Target.DBus() == config.DBusPrivate {
		cmd = append(cmd, "dbus-launch")
	}
	cmd = append(cmd, Workload(eff.Exec, extra, eff.DefaultArgs, paths)...)
	return cmd
}

// LaunchSandboxArgv is the `psbox sandbox … -- <command>` argument list (no argv0).
func LaunchSandboxArgv(eff *config.Effective, extra []string, paths config.Paths) []string {
	args := FlagsFromApplication(eff.Target, paths).Args()
	args = append(args, "--")
	args = append(args, TrailingCommand(eff, extra, paths)...)
	return args
}

func envSetenv(env map[string]string) []string {
	if len(env) == 0 {
		return nil
	}
	keys := make([]string, 0, len(env))
	for k := range env {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	var out []string
	for _, k := range keys {
		out = append(out, "--setenv", k, env[k])
	}
	return out
}
