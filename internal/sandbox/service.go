package sandbox

// Sandbox verb: compose bwrap from flags and exec or print.

import (
	"fmt"
	"io"
	"os"

	"petris.dev/psbox/internal/bwrap"
	"petris.dev/psbox/internal/config"
)

// Request is one `psbox sandbox` invocation.
type Request struct {
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
	Cache         string
	Downloads     bool
	Print         bool
	Command       []string
}

// Service implements `psbox sandbox`.
type Service struct {
	paths config.Paths
}

// NewService returns a sandbox service.
func NewService(paths config.Paths) *Service {
	return &Service{paths: paths}
}

// Run prints or executes the composed bwrap command.
func (s *Service) Run(req Request, stdout io.Writer) error {
	if req.Home != "" && (req.HomeTmpfs || req.NoHome) {
		return fmt.Errorf("--home, --home-tmpfs, and --no-home are mutually exclusive")
	}
	if req.HomeTmpfs && req.NoHome {
		return fmt.Errorf("--home, --home-tmpfs, and --no-home are mutually exclusive")
	}
	if req.Home == "" && !req.HomeTmpfs && !req.NoHome {
		return fmt.Errorf("one of --home, --home-tmpfs, or --no-home is required")
	}
	if req.Home != "" {
		info, err := os.Stat(req.Home)
		if err == nil && !info.IsDir() {
			return fmt.Errorf("home path %s is not a directory", req.Home)
		}
	}
	if req.NoDBus && req.NoSessionDBus {
		return fmt.Errorf("--no-dbus and --no-session-dbus are mutually exclusive")
	}
	switch req.Cache {
	case "", config.CacheTmpfs:
	case config.CacheHome:
	default:
		return fmt.Errorf("invalid --cache %q; use tmpfs or home", req.Cache)
	}
	if len(req.Command) == 0 {
		return fmt.Errorf("missing command; use: psbox sandbox [flags] -- <command>")
	}

	flags := bwrap.Flags{
		Home:          req.Home,
		HomeTmpfs:     req.HomeTmpfs,
		NoHome:        req.NoHome,
		NoAudio:       req.NoAudio,
		NoVideo:       req.NoVideo,
		NoDBus:        req.NoDBus,
		NoSessionDBus: req.NoSessionDBus,
		NoFontconfig:  req.NoFontconfig,
		NoUdev:        req.NoUdev,
		NoEvdev:       req.NoEvdev,
		NoDisplay:     req.NoDisplay,
		CacheHome:     req.Cache == config.CacheHome,
		Downloads:     req.Downloads,
	}
	env := bwrap.EnvFromPaths(s.paths)
	argv := bwrap.Argv(flags, env, req.Command)
	if req.Print {
		_, err := fmt.Fprintln(stdout, bwrap.Quote(argv))
		return err
	}
	return bwrap.Exec(flags, env, req.Command)
}
