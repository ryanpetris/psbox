package launch

// Launch verb: print or exec a composed sandbox.

import (
	"context"
	"fmt"
	"io"
	"os"
	"path/filepath"

	"petris.dev/psbox/internal/bwrap"
	"petris.dev/psbox/internal/config"
	"petris.dev/psbox/internal/instance"
)

// Request is one `psbox launch` invocation.
type Request struct {
	Application string
	Args        []string
	Command     bool
	Objects     string
	Isolation   string
	Instance    string
	Print       bool
	PrintBwrap  bool
	ProgramName string
}

// Service implements `psbox launch`.
type Service struct {
	loader *config.Loader
	paths  config.Paths
	inst   *instance.Client
}

// NewService returns a launch service bound to the process paths.
func NewService(loader *config.Loader, paths config.Paths, inst *instance.Client) *Service {
	return &Service{loader: loader, paths: paths, inst: inst}
}

// Run prints or executes the requested launch.
func (s *Service) Run(ctx context.Context, req Request, stdout io.Writer) error {
	if req.Print && req.PrintBwrap {
		return fmt.Errorf("--print and --print-bwrap cannot be used together")
	}
	if req.Command && len(req.Args) == 0 {
		return fmt.Errorf("--command requires a command")
	}
	if err := config.CheckName("application", req.Application); err != nil {
		return err
	}

	override, hasOverride, err := config.IsolationFromFlags(req.Isolation, req.Instance)
	if err != nil {
		return err
	}

	paths := s.paths.WithObjectPath(req.Objects)
	col, err := s.loader.LoadPath(paths.ObjectPath, paths)
	if err != nil {
		return err
	}
	paths = paths.WithCollection(col)
	eff, err := col.Resolve(req.Application, override, hasOverride)
	if err != nil {
		return err
	}
	if req.Objects != "" && req.Objects != s.paths.ObjectPath && eff.Isolation.Instance() && !req.Print && !req.PrintBwrap {
		return fmt.Errorf("--objects cannot be used with instance isolation")
	}

	extra := req.Args
	if req.Command {
		eff.Exec = append([]string(nil), req.Args...)
		eff.DefaultArgs = nil
		extra = nil
	}

	sandboxArgs := bwrap.LaunchSandboxArgv(eff, extra, paths)
	flags := bwrap.FlagsFromApplication(eff.Target, paths)
	env := bwrap.EnvFromPaths(paths)
	if _, err := bwrap.ComputeHashes(eff.Target, paths, env); err != nil {
		return err
	}
	trailing := bwrap.TrailingCommand(eff, extra, paths)
	bwrapArgv := bwrap.Argv(flags, env, trailing)

	prog := req.ProgramName
	if prog == "" {
		prog = "psbox"
	}
	prog = filepath.Base(prog)

	if req.Print {
		_, err := fmt.Fprintln(stdout, bwrap.Quote(append([]string{prog, "sandbox"}, sandboxArgs...)))
		return err
	}
	if req.PrintBwrap {
		_, err := fmt.Fprintln(stdout, bwrap.Quote(bwrapArgv))
		return err
	}
	if eff.Isolation.Instance() {
		if s.inst == nil {
			return fmt.Errorf("instance isolation is not available")
		}
		cwd, err := os.Getwd()
		if err != nil {
			cwd = "/"
		}
		return s.inst.Run(ctx, instance.Request{
			Effective: eff,
			Paths:     paths,
			Argv:      bwrap.Workload(eff.Exec, extra, eff.DefaultArgs, paths),
			Env:       instanceEnviron(eff.Env),
			Cwd:       cwd,
		})
	}
	return bwrap.Exec(flags, env, trailing)
}

func instanceEnviron(overlay map[string]string) map[string]string {
	env := map[string]string{}
	for _, kv := range os.Environ() {
		for i := 0; i < len(kv); i++ {
			if kv[i] == '=' {
				env[kv[:i]] = kv[i+1:]
				break
			}
		}
	}
	for k, v := range overlay {
		env[k] = v
	}
	return env
}
