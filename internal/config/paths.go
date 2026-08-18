package config

// Host and psbox path resolution from the process environment.

import (
	"fmt"
	"os"
	"os/user"
	"path/filepath"
	"strings"
)

const (
	EnvObjectPath = "PSBOX_OBJECT_PATH"
	EnvRoot       = "PSBOX_ROOT"
	EnvAgent      = "PSBOX_AGENT_BIN"

	DefaultRootRelative      = "Sandbox"
	DefaultObjectDirRelative = "psbox/objects"
	DefaultAgentPath         = "/usr/bin/psboxa"
	DefaultWaylandDisplay    = "wayland-0"
	DefaultPipewireCore      = "pipewire-0"
	DefaultPATH              = "/usr/local/bin:/usr/bin"
)

// Paths is the process environment psbox consults for objects, homes, and display.
type Paths struct {
	Home           string
	ConfigHome     string
	DataHome       string
	RuntimeDir     string
	ObjectPath     string
	Root           string
	Agent          string
	WaylandDisplay string
	Xauthority     string
	PipewireCore   string
	DataDirs       []string
	ConfigDirs     []string
}

// LoadPaths reads path and display settings from env.
func LoadPaths() (Paths, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		if u, uerr := user.Current(); uerr == nil && u.HomeDir != "" {
			home = u.HomeDir
		} else {
			return Paths{}, fmt.Errorf("resolve home directory: %w", err)
		}
	}

	configHome := envPath("XDG_CONFIG_HOME", filepath.Join(home, ".config"))
	dataHome := envPath("XDG_DATA_HOME", filepath.Join(home, ".local", "share"))
	runtimeDir := RuntimeDir()

	p := Paths{
		Home:           home,
		ConfigHome:     configHome,
		DataHome:       dataHome,
		RuntimeDir:     runtimeDir,
		ObjectPath:     envPath(EnvObjectPath, filepath.Join(configHome, DefaultObjectDirRelative)),
		Root:           envPath(EnvRoot, filepath.Join(home, DefaultRootRelative)),
		Agent:          envPath(EnvAgent, DefaultAgentPath),
		WaylandDisplay: envOr("WAYLAND_DISPLAY", DefaultWaylandDisplay),
		Xauthority:     os.Getenv("XAUTHORITY"),
		PipewireCore:   envOr("PIPEWIRE_CORE", DefaultPipewireCore),
		DataDirs:       envPathList("XDG_DATA_DIRS", "/usr/local/share:/usr/share"),
		ConfigDirs:     envPathList("XDG_CONFIG_DIRS", "/etc/xdg"),
	}
	return p, nil
}

// WithObjectPath returns a copy using objectPath when it is not empty.
func (p Paths) WithObjectPath(objectPath string) Paths {
	if objectPath != "" {
		p.ObjectPath = objectPath
	}
	return p
}

// WithCollection applies sandbox_root and object path from a loaded collection.
func (p Paths) WithCollection(col *Collection) Paths {
	if col == nil {
		return p
	}
	p.Root = col.Root
	p.ObjectPath = col.ObjectPath
	return p
}

// ReplaceTokens expands :root:, :home:, and a leading ~ in val.
func (p Paths) ReplaceTokens(val string) string {
	val = strings.ReplaceAll(val, RootToken, p.Root)
	val = strings.ReplaceAll(val, HomeToken, p.Home)
	return expandUser(val, p.Home)
}

// ExpandHome expands a configured home value. Empty means $PSBOX_ROOT/<name>.
func (p Paths) ExpandHome(name, home string) string {
	if home == "" {
		return filepath.Join(p.Root, name)
	}
	if home == HomeNone || home == HomeTmpfs {
		return home
	}
	return p.ReplaceTokens(home)
}

// ImplicitHome is the default private home for an application name.
func (p Paths) ImplicitHome(name string) string {
	return filepath.Join(p.Root, name)
}

// RuntimeDir is $XDG_RUNTIME_DIR, or /run/user/<uid> when unset.
func RuntimeDir() string {
	return envPath("XDG_RUNTIME_DIR", DefaultRuntimeDir())
}

// DefaultRuntimeDir is the Arch/systemd fallback for XDG_RUNTIME_DIR.
func DefaultRuntimeDir() string {
	return filepath.Join("/run/user", fmt.Sprint(os.Getuid()))
}

func envOr(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}

func envPath(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}

func envPathList(key, fallback string) []string {
	raw := os.Getenv(key)
	if raw == "" {
		raw = fallback
	}
	var out []string
	for _, part := range strings.Split(raw, ":") {
		if part != "" {
			out = append(out, part)
		}
	}
	return out
}

func expandUser(val, home string) string {
	if val == "~" {
		return home
	}
	if strings.HasPrefix(val, "~/") {
		return filepath.Join(home, val[2:])
	}
	return val
}
