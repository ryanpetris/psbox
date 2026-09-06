package instance

// Host daemon: socket activation and serialized sandbox launch decisions.

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"os"
	"os/exec"
	"strconv"
	"sync"
	"syscall"
	"time"

	"golang.org/x/sys/unix"

	"petris.dev/psbox/internal/bwrap"
	"petris.dev/psbox/internal/config"
)

const listenFD = 3

// Daemon is psboxd for one (sandbox, instance).
type Daemon struct {
	log      *slog.Logger
	loader   *config.Loader
	paths    config.Paths
	openURI  func(context.Context, string) error
	command  func(context.Context, string, ...string) *exec.Cmd
	sandbox  string
	instance string
	launchMu sync.Mutex
	mu       sync.Mutex
	active   *sandboxProcess
	clients  int
}

// NewDaemon returns a host instance daemon.
func NewDaemon(log *slog.Logger, loader *config.Loader, paths config.Paths) *Daemon {
	if log == nil {
		log = slog.Default()
	}
	return &Daemon{log: log, loader: loader, paths: paths, openURI: hostOpenURI, command: exec.CommandContext}
}

// Run is the psboxd process. identity is the escaped systemd %i value.
func (d *Daemon) Run(ctx context.Context, identity string) error {
	if err := validateListenFDs(); err != nil {
		return err
	}
	raw, err := Unescape(identity)
	if err != nil {
		return err
	}
	sandbox, inst, err := SplitIdentity(raw)
	if err != nil {
		return err
	}
	d.sandbox, d.instance = sandbox, inst

	file := os.NewFile(listenFD, "listen")
	if file == nil {
		return fmt.Errorf("missing listen fd")
	}
	unix.CloseOnExec(listenFD)
	ln, err := net.FileListener(file)
	_ = file.Close()
	if err != nil {
		return fmt.Errorf("listen fd: %w", err)
	}
	defer ln.Close()
	uln, ok := ln.(*net.UnixListener)
	if !ok {
		return fmt.Errorf("listen fd is not a unix socket")
	}

	ctx, cancel := context.WithCancel(ctx)
	var clients sync.WaitGroup
	stop := context.AfterFunc(ctx, func() { _ = ln.Close() })
	defer func() {
		cancel()
		stop()
		_ = ln.Close()
		clients.Wait()
		d.mu.Lock()
		active := d.active
		d.mu.Unlock()
		if active != nil {
			active.stop()
		}
	}()
	for {
		if ctx.Err() != nil {
			return ctx.Err()
		}
		if err := uln.SetDeadline(time.Now().Add(AcceptIdleTimeout)); err != nil {
			return err
		}
		conn, err := uln.AcceptUnix()
		if err != nil {
			if ne, ok := err.(net.Error); ok && ne.Timeout() {
				d.mu.Lock()
				idle := d.clients == 0 && (d.active == nil || d.active.finished())
				d.mu.Unlock()
				if idle {
					return nil
				}
				continue
			}
			if ctx.Err() != nil {
				return ctx.Err()
			}
			return fmt.Errorf("accept client: %w", err)
		}
		d.mu.Lock()
		d.clients++
		d.mu.Unlock()
		clients.Go(func() {
			defer func() { d.mu.Lock(); d.clients--; d.mu.Unlock() }()
			d.serveClient(ctx, conn)
		})
	}
}

func (d *Daemon) serveClient(ctx context.Context, conn *net.UnixConn) {
	defer conn.Close()
	stop := context.AfterFunc(ctx, func() { _ = conn.Close() })
	defer stop()
	if err := conn.SetReadDeadline(time.Now().Add(SpawnReadTimeout)); err != nil {
		return
	}
	msg, files, err := ReadMsg(conn, 3)
	if err != nil {
		return
	}
	defer func() { closeFiles(files) }()
	if err := conn.SetReadDeadline(time.Time{}); err != nil {
		return
	}
	var invalid string
	switch {
	case msg.Type != TypeSpawn:
		invalid = "expected spawn"
	case msg.Protocol != 0 && msg.Protocol != ProtocolVersion:
		invalid = "unsupported protocol"
	case msg.Sandbox != "" && msg.Sandbox != d.sandbox:
		invalid = "sandbox mismatch"
	}
	if invalid != "" {
		_ = WriteMsg(conn, Message{Type: TypeSpawnError, Message: invalid}, nil)
		return
	}

	// A launch decision and its registered spawn form one operation. Another
	// launch cannot rebuild an idle sandbox between these two steps.
	d.launchMu.Lock()
	run, err := d.ensureSandbox(ctx, msg)
	var id string
	var replies chan Message
	if err == nil {
		id, replies, err = run.request(msg, files)
	}
	d.launchMu.Unlock()
	if err != nil {
		_ = WriteMsg(conn, Message{Type: TypeSpawnError, Message: err.Error()}, nil)
		return
	}
	defer run.unwaiter(id)
	closeFiles(files)
	files = nil
	cfg, running := run.hashes.Hex()

	var spawned Message
	select {
	case reply, ok := <-replies:
		if !ok {
			_ = WriteMsg(conn, Message{Type: TypeSpawnError, Message: "sandbox agent disconnected"}, nil)
			return
		}
		spawned = reply
	case <-ctx.Done():
		return
	case <-time.After(FirstReplyTimeout):
		_ = WriteMsg(run.conn, Message{Type: TypeSignal, ID: id, Signum: int(syscall.SIGHUP)}, nil)
		_ = WriteMsg(conn, Message{Type: TypeSpawnError, Message: "agent spawn timeout"}, nil)
		return
	}
	spawned.ConfigHash, spawned.RunningHash = cfg, running
	if spawned.Type != TypeSpawned && spawned.Type != TypeSpawnError {
		_ = WriteMsg(conn, Message{Type: TypeSpawnError, Message: "unexpected agent spawn reply"}, nil)
		return
	}
	if err := WriteMsg(conn, spawned, nil); err != nil {
		_ = WriteMsg(run.conn, Message{Type: TypeSignal, ID: id, Signum: int(syscall.SIGHUP)}, nil)
		return
	}
	if spawned.Type == TypeSpawnError {
		return
	}

	readerDone := make(chan struct{})
	go func() {
		defer close(readerDone)
		for {
			msg, _, err := ReadMsg(conn, 0)
			if err != nil {
				return
			}
			if msg.Type == TypeSignal {
				msg.ID = id
				if err := WriteMsg(run.conn, msg, nil); err != nil {
					return
				}
			}
		}
	}()
	defer func() { _ = conn.Close(); <-readerDone }()
	select {
	case exited, ok := <-replies:
		if !ok {
			exited = Message{Type: TypeSpawnError, ID: id, Message: "sandbox agent disconnected"}
		}
		_ = WriteMsg(conn, exited, nil)
	case <-readerDone:
		_ = WriteMsg(run.conn, Message{Type: TypeSignal, ID: id, Signum: int(syscall.SIGHUP)}, nil)
	case <-ctx.Done():
		_ = WriteMsg(run.conn, Message{Type: TypeSignal, ID: id, Signum: int(syscall.SIGHUP)}, nil)
	}
}

// ensureSandbox runs with launchMu held. The original process paths remain the
// reload baseline so removing a collection option restores its process default.
func (d *Daemon) ensureSandbox(ctx context.Context, spawn Message) (*sandboxProcess, error) {
	col, err := d.loader.LoadPath(d.paths.ObjectPath, d.paths)
	if err != nil {
		return nil, err
	}
	app, err := col.RequireApplication(d.sandbox)
	if err != nil {
		return nil, err
	}
	if app.SandboxTarget() != "" {
		return nil, fmt.Errorf("application %q is a sandbox referrer", d.sandbox)
	}
	paths := d.paths.WithCollection(col)
	env := composeEnvFromSpawn(spawn.Env, paths)
	hashes, err := bwrap.ComputeHashes(app, paths, env)
	if err != nil {
		return nil, err
	}
	d.mu.Lock()
	active := d.active
	d.mu.Unlock()
	if active != nil && !active.finished() {
		if hashesEqual(active.hashes, hashes) {
			return active, nil
		}
		if active.workload(ctx) {
			d.log.Warn("instance hashes differ; joining because workload is present")
			return active, nil
		}
	}
	if active != nil {
		active.stop()
	}
	run, err := d.startSandbox(ctx, app, paths, env, hashes)
	if err != nil {
		return nil, err
	}
	d.mu.Lock()
	d.active = run
	d.mu.Unlock()
	return run, nil
}

func hashesEqual(a, b bwrap.Hashes) bool {
	ac, ar := a.Hex()
	bc, br := b.Hex()
	return ac == bc && ar == br
}

func validateListenFDs() error {
	n, err := strconv.Atoi(os.Getenv("LISTEN_FDS"))
	if err != nil || n != 1 {
		return fmt.Errorf("psboxd requires systemd socket activation")
	}
	pid, err := strconv.Atoi(os.Getenv("LISTEN_PID"))
	if err != nil || pid != os.Getpid() {
		return fmt.Errorf("LISTEN_PID does not match this process")
	}
	return nil
}
