package bwrap

// Compose the options-side bwrap argv from sandbox flags and host env.

import (
	"os"
	"path/filepath"
	"sort"
)

const BwrapPath = "/usr/bin/bwrap"

// Compose builds the bwrap option flags (no trailing command).
func Compose(f Flags, env Env) []string {
	var args []string
	args = append(args, "--die-with-parent", "--unshare-pid")
	args = append(args, "--proc", "/proc")
	args = append(args, "--dev", "/dev")
	args = append(args, "--tmpfs", "/tmp")
	args = append(args, bindTryRO("/etc")...)
	args = append(args, bindTryRO("/opt")...)
	args = append(args, bindTryRO("/sys")...)
	args = append(args, bindTryRO("/usr")...)
	args = append(args, "--symlink", "usr/bin", "/bin")
	args = append(args, "--symlink", "usr/bin", "/sbin")
	args = append(args, "--symlink", "usr/lib", "/lib")
	args = append(args, "--symlink", "usr/lib", "/lib64")
	args = append(args, bindTryRO("/var/empty")...)
	if env.RuntimeDir != "" {
		args = append(args, "--tmpfs", env.RuntimeDir)
	}
	args = append(args, bindTryRO("/run/systemd/resolve")...)

	if f.Home != "" {
		dest := env.Home
		if dest == "" {
			dest = f.Home
		}
		args = append(args, "--bind", absPath(f.Home), dest)
		if !f.CacheHome {
			args = append(args, "--tmpfs", filepath.Join(dest, ".cache"))
		}
		args = append(args, downloadsBind(f, env, dest)...)
	} else if f.HomeTmpfs {
		dest := env.Home
		if dest == "" {
			dest = "/tmp/home"
		}
		args = append(args, "--tmpfs", dest)
		args = append(args, downloadsBind(f, env, dest)...)
	}

	if !f.NoAudio {
		args = append(args, bindTryDev("/dev/snd")...)
		if env.RuntimeDir != "" {
			args = append(args, bindTryRO(filepath.Join(env.RuntimeDir, "pulse"))...)
			core := env.PipewireCore
			if core == "" {
				core = "pipewire-0"
			}
			args = append(args, bindTryRO(filepath.Join(env.RuntimeDir, core))...)
		}
	}
	if !f.NoVideo {
		args = append(args, bindTryDev("/dev/v4l")...)
		for _, dev := range globCharDevices("/dev/video*") {
			args = append(args, bindTryDev(dev)...)
		}
	}
	if !f.NoDBus {
		args = append(args, bindTryRO("/var/lib/dbus")...)
		args = append(args, bindTryRO("/run/dbus")...)
		if !f.NoSessionDBus && env.RuntimeDir != "" {
			args = append(args, bindTryRO(filepath.Join(env.RuntimeDir, "bus"))...)
		}
	}
	if !f.NoFontconfig {
		args = append(args, bindTryRO("/var/cache/fontconfig")...)
	}
	if !f.NoUdev {
		args = append(args, bindTryRO("/run/udev")...)
	}
	if !f.NoEvdev {
		args = append(args, bindTryDev("/dev/input")...)
		for _, dev := range globCharDevices("/dev/hidraw*") {
			args = append(args, bindTryDev(dev)...)
		}
	}
	if !f.NoDisplay {
		args = append(args, bindTryDev("/dev/dri")...)
		wayland := env.WaylandDisplay
		if wayland == "" {
			wayland = "wayland-0"
		}
		if env.RuntimeDir != "" {
			args = append(args, bindTryRO(filepath.Join(env.RuntimeDir, wayland))...)
		}
		args = append(args, bindTryRO("/tmp/.ICE-unix")...)
		args = append(args, bindTryRO("/tmp/.X11-unix")...)
		if env.Xauthority != "" {
			args = append(args, bindTryRO(env.Xauthority)...)
		}
		args = append(args, "--setenv", "WAYLAND_DISPLAY", wayland)
		if env.Xauthority != "" {
			args = append(args, "--setenv", "XAUTHORITY", env.Xauthority)
		}
	}
	return args
}

// Argv is the full bwrap command: bwrap, composed flags, then trailing tokens.
func Argv(f Flags, env Env, trailing []string) []string {
	args := []string{BwrapPath}
	args = append(args, Compose(f, env)...)
	args = append(args, trailing...)
	return args
}

func downloadsBind(f Flags, env Env, dest string) []string {
	if !f.Downloads || dest == "" || env.Home == "" {
		return nil
	}
	src := filepath.Join(env.Home, "Downloads")
	return []string{"--bind", absPath(src), filepath.Join(dest, "Downloads")}
}

func bindTryRO(path string) []string {
	return []string{"--ro-bind-try", path, path}
}

func bindTryDev(path string) []string {
	return []string{"--dev-bind-try", path, path}
}

func absPath(path string) string {
	abs, err := filepath.Abs(path)
	if err != nil {
		return path
	}
	return abs
}

func globCharDevices(pattern string) []string {
	matches, err := filepath.Glob(pattern)
	if err != nil {
		return nil
	}
	var out []string
	for _, match := range matches {
		info, err := os.Stat(match)
		if err != nil {
			continue
		}
		if info.Mode()&os.ModeCharDevice != 0 {
			out = append(out, match)
		}
	}
	sort.Strings(out)
	return out
}
