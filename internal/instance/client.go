package instance

// Host client: start the socket unit, spawn once, relay signals.

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"os"
	"os/signal"
	"syscall"
	"time"

	"petris.dev/psbox/internal/bwrap"
	"petris.dev/psbox/internal/config"
)

// Starter starts a systemd user socket unit.
type Starter interface {
	Start(ctx context.Context, unit string) error
}

// Client is the psbox instance launcher.
type Client struct {
	log     *slog.Logger
	paths   config.Paths
	starter Starter
}

// NewClient returns an instance client.
func NewClient(log *slog.Logger, paths config.Paths, starter Starter) *Client {
	if log == nil {
		log = slog.Default()
	}
	return &Client{log: log, paths: paths, starter: starter}
}

// Request is one instance-mode launch.
type Request struct {
	Effective *config.Effective
	Paths     config.Paths
	Argv      []string
	Env       map[string]string
	Cwd       string
}

// Run connects to the instance, sends one spawn, and returns the exit status.
func (c *Client) Run(ctx context.Context, req Request) error {
	if req.Effective == nil {
		return fmt.Errorf("missing application")
	}
	inst := req.Effective.Isolation.Name
	if inst == "" {
		inst = config.DefaultInstance
	}
	ident := Identity(req.Effective.SandboxName, inst)
	escaped := Escape(ident)
	path := SocketPath(req.Paths.RuntimeDir, escaped)
	if err := CheckSocketPath(path); err != nil {
		return err
	}
	socketUnit, serviceUnit := UnitNames(escaped)

	conn, err := c.connect(ctx, path, socketUnit, serviceUnit)
	if err != nil {
		return err
	}
	defer conn.Close()

	cwd := req.Cwd
	if cwd == "" {
		cwd, err = os.Getwd()
		if err != nil {
			cwd = "/"
		}
	}
	env := req.Env
	if env == nil {
		env = environMap()
	}
	for k, v := range req.Effective.Env {
		env[k] = v
	}

	local, err := bwrap.ComputeHashes(req.Effective.Target, req.Paths, composeEnvFromSpawn(env, req.Paths))
	if err != nil {
		return err
	}
	localCfg, localRun := local.Hex()

	stdio := []*os.File{os.Stdin, os.Stdout, os.Stderr}
	if err := WriteMsg(conn, Message{
		Type:     TypeSpawn,
		Protocol: ProtocolVersion,
		Sandbox:  req.Effective.SandboxName,
		Instance: inst,
		Argv:     req.Argv,
		Env:      env,
		Cwd:      cwd,
	}, stdio); err != nil {
		return err
	}

	_ = conn.SetReadDeadline(time.Now().Add(FirstReplyTimeout))
	reply, _, err := ReadMsg(conn, 0)
	if err != nil {
		return fmt.Errorf("waiting for spawn reply: %w", err)
	}
	_ = conn.SetReadDeadline(time.Time{})

	switch reply.Type {
	case TypeSpawnError:
		return fmt.Errorf("spawn failed: %s", reply.Message)
	case TypeSpawned:
		if reply.ConfigHash != localCfg || reply.RunningHash != localRun {
			c.log.Warn("running instance hashes differ from this client",
				"local_config_hash", localCfg,
				"instance_config_hash", reply.ConfigHash,
				"local_running_hash", localRun,
				"instance_running_hash", reply.RunningHash,
			)
		}
	default:
		return fmt.Errorf("unexpected spawn reply %q", reply.Type)
	}

	_ = os.Stdin.Close()
	_ = os.Stdout.Close()

	sigc := make(chan os.Signal, 8)
	signal.Notify(sigc, syscall.SIGINT, syscall.SIGTERM, syscall.SIGQUIT, syscall.SIGHUP, syscall.SIGTSTP, syscall.SIGCONT)
	defer signal.Stop(sigc)

	errc := make(chan error, 1)
	go func() {
		msg, _, err := ReadMsg(conn, 0)
		if err != nil {
			errc <- err
			return
		}
		errc <- exitedError(msg)
	}()

	for {
		select {
		case <-ctx.Done():
			_ = WriteMsg(conn, Message{Type: TypeSignal, Signum: int(syscall.SIGHUP)}, nil)
			return ctx.Err()
		case sig := <-sigc:
			sys, ok := sig.(syscall.Signal)
			if !ok {
				continue
			}
			_ = WriteMsg(conn, Message{Type: TypeSignal, Signum: int(sys)}, nil)
			if sys == syscall.SIGTSTP {
				_ = syscall.Kill(os.Getpid(), syscall.SIGSTOP)
			}
		case err := <-errc:
			return err
		}
	}
}

func (c *Client) connect(ctx context.Context, path, socketUnit, serviceUnit string) (*net.UnixConn, error) {
	var last error
	for attempt := 0; attempt < 2; attempt++ {
		if err := c.starter.Start(ctx, socketUnit); err != nil {
			last = fmt.Errorf("start %s: %w (see journalctl --user -u %s)", socketUnit, err, serviceUnit)
			continue
		}
		conn, err := dialUnixPacket(ctx, path)
		if err == nil {
			return conn, nil
		}
		last = fmt.Errorf("connect %s: %w (see journalctl --user -u %s)", path, err, serviceUnit)
	}
	if last == nil {
		last = fmt.Errorf("connect %s (see journalctl --user -u %s)", path, serviceUnit)
	}
	return nil, last
}

func dialUnixPacket(ctx context.Context, path string) (*net.UnixConn, error) {
	d := net.Dialer{Timeout: FirstReplyTimeout}
	conn, err := d.DialContext(ctx, "unixpacket", path)
	if err != nil {
		return nil, err
	}
	uc, ok := conn.(*net.UnixConn)
	if !ok {
		_ = conn.Close()
		return nil, fmt.Errorf("connection is not a unix socket")
	}
	return uc, nil
}

func composeEnvFromSpawn(env map[string]string, paths config.Paths) bwrap.Env {
	ce := bwrap.EnvFromPaths(paths)
	if v, ok := env["WAYLAND_DISPLAY"]; ok {
		ce.WaylandDisplay = v
	}
	if v, ok := env["XAUTHORITY"]; ok {
		ce.Xauthority = v
	}
	return ce
}

func environMap() map[string]string {
	out := map[string]string{}
	for _, kv := range os.Environ() {
		k, v, ok := splitEnv(kv)
		if ok {
			out[k] = v
		}
	}
	return out
}

func splitEnv(kv string) (string, string, bool) {
	for i := 0; i < len(kv); i++ {
		if kv[i] == '=' {
			return kv[:i], kv[i+1:], true
		}
	}
	return "", "", false
}

func exitedError(msg Message) error {
	if msg.Type != TypeExited {
		return fmt.Errorf("unexpected message %q", msg.Type)
	}
	if msg.Signal != nil && *msg.Signal != 0 {
		return &StatusError{Code: 128 + *msg.Signal}
	}
	code := 0
	if msg.Code != nil {
		code = *msg.Code
	}
	if code == 0 {
		return nil
	}
	return &StatusError{Code: code}
}

var _ error = (*StatusError)(nil)
