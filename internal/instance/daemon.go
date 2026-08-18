package instance

// Host daemon: socket activation, compose/join/rebuild, xdg-open.

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
	log     *slog.Logger
	loader  *config.Loader
	paths   config.Paths
	openURI func(uri string) error

	sandbox  string
	instance string

	mu      sync.Mutex
	app     *config.Application
	hashes  bwrap.Hashes
	bwrap   *exec.Cmd
	agent   *net.UnixConn
	seq     uint64
	waiters map[string]chan Message
	clients int
}

// NewDaemon returns a host instance daemon.
func NewDaemon(log *slog.Logger, loader *config.Loader, paths config.Paths) *Daemon {
	if log == nil {
		log = slog.Default()
	}
	return &Daemon{
		log:     log,
		loader:  loader,
		paths:   paths,
		openURI: hostOpenURI,
		waiters: map[string]chan Message{},
	}
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
	d.sandbox = sandbox
	d.instance = inst

	col, err := d.loader.LoadPath(d.paths.ObjectPath, d.paths)
	if err != nil {
		return err
	}
	d.paths = d.paths.WithCollection(col)
	app, err := col.LookupApplication(sandbox)
	if err != nil {
		return err
	}
	if app.SandboxTarget() != "" {
		return fmt.Errorf("application %q is a sandbox referrer", sandbox)
	}
	if _, err := col.RequireApplication(sandbox); err != nil {
		return err
	}
	d.app = app

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

	go d.readAgent(ctx)

	for {
		if ctx.Err() != nil {
			d.stopSandbox()
			return ctx.Err()
		}
		// Always poll accept so sandbox exit and a finished client can
		// wake the loop. An infinite deadline would leave psboxd stuck
		// after the last connection or after bwrap exits.
		_ = uln.SetDeadline(time.Now().Add(AcceptIdleTimeout))
		conn, err := uln.AcceptUnix()
		if err != nil {
			if ne, ok := err.(net.Error); ok && ne.Timeout() {
				d.mu.Lock()
				idle := d.bwrap == nil && d.clients == 0
				d.mu.Unlock()
				if idle {
					return nil
				}
				continue
			}
			if ctx.Err() != nil {
				return ctx.Err()
			}
			d.log.Error("accept", "error", err)
			continue
		}
		d.mu.Lock()
		d.clients++
		d.mu.Unlock()
		go func() {
			defer func() {
				d.mu.Lock()
				d.clients--
				d.mu.Unlock()
			}()
			d.serveClient(ctx, conn)
		}()
	}
}

func (d *Daemon) serveClient(ctx context.Context, conn *net.UnixConn) {
	defer conn.Close()
	_ = conn.SetReadDeadline(time.Now().Add(SpawnReadTimeout))
	msg, files, err := ReadMsg(conn, 3)
	if err != nil {
		return
	}
	_ = conn.SetReadDeadline(time.Time{})
	if msg.Type != TypeSpawn {
		closeFiles(files)
		_ = WriteMsg(conn, Message{Type: TypeSpawnError, Message: "expected spawn"}, nil)
		return
	}
	if msg.Protocol != 0 && msg.Protocol != ProtocolVersion {
		closeFiles(files)
		_ = WriteMsg(conn, Message{Type: TypeSpawnError, Message: "unsupported protocol"}, nil)
		return
	}
	if msg.Sandbox != "" && msg.Sandbox != d.sandbox {
		closeFiles(files)
		_ = WriteMsg(conn, Message{Type: TypeSpawnError, Message: "sandbox mismatch"}, nil)
		return
	}

	id, err := d.ensureSandbox(msg)
	if err != nil {
		closeFiles(files)
		cfg, run := d.hashHex()
		_ = WriteMsg(conn, Message{Type: TypeSpawnError, Message: err.Error(), ConfigHash: cfg, RunningHash: run}, nil)
		return
	}

	msg.ID = id
	if err := WriteMsg(d.agentConn(), msg, files); err != nil {
		closeFiles(files)
		_ = WriteMsg(conn, Message{Type: TypeSpawnError, Message: err.Error()}, nil)
		return
	}
	closeFiles(files)

	ch := d.waiter(id)
	defer d.unwaiter(id)

	var spawned Message
	select {
	case spawned = <-ch:
	case <-ctx.Done():
		return
	case <-time.After(FirstReplyTimeout):
		_ = WriteMsg(conn, Message{Type: TypeSpawnError, Message: "agent spawn timeout"}, nil)
		return
	}
	if spawned.Type == TypeSpawnError {
		cfg, run := d.hashHex()
		spawned.ConfigHash = cfg
		spawned.RunningHash = run
		_ = WriteMsg(conn, spawned, nil)
		return
	}
	cfg, run := d.hashHex()
	spawned.ConfigHash = cfg
	spawned.RunningHash = run
	if err := WriteMsg(conn, spawned, nil); err != nil {
		_ = WriteMsg(d.agentConn(), Message{Type: TypeSignal, ID: id, Signum: int(syscall.SIGHUP)}, nil)
		return
	}

	errc := make(chan error, 1)
	go func() {
		for {
			msg, _, err := ReadMsg(conn, 0)
			if err != nil {
				errc <- err
				return
			}
			if msg.Type == TypeSignal {
				msg.ID = id
				_ = WriteMsg(d.agentConn(), msg, nil)
			}
		}
	}()

	select {
	case exited := <-ch:
		_ = WriteMsg(conn, exited, nil)
	case <-errc:
		_ = WriteMsg(d.agentConn(), Message{Type: TypeSignal, ID: id, Signum: int(syscall.SIGHUP)}, nil)
	case <-ctx.Done():
		_ = WriteMsg(d.agentConn(), Message{Type: TypeSignal, ID: id, Signum: int(syscall.SIGHUP)}, nil)
	}
}

func (d *Daemon) ensureSandbox(spawn Message) (string, error) {
	d.mu.Lock()
	defer d.mu.Unlock()

	env := composeEnvFromSpawn(spawn.Env, d.paths)
	hashes, err := bwrap.ComputeHashes(d.app, d.paths, env)
	if err != nil {
		return "", err
	}

	if d.bwrap == nil {
		if err := d.startSandboxLocked(env); err != nil {
			return "", err
		}
		d.hashes = hashes
		return d.nextIDLocked(), nil
	}

	same := hashesEqual(d.hashes, hashes)
	if !same {
		if d.queryWorkloadLocked() {
			d.log.Error("instance hashes differ; joining because workload is present")
		} else {
			d.stopSandboxLocked()
			if err := d.startSandboxLocked(env); err != nil {
				return "", err
			}
			d.hashes = hashes
			return d.nextIDLocked(), nil
		}
	}
	return d.nextIDLocked(), nil
}

func (d *Daemon) startSandboxLocked(env bwrap.Env) error {
	fds, err := unix.Socketpair(unix.AF_UNIX, unix.SOCK_SEQPACKET|unix.SOCK_CLOEXEC, 0)
	if err != nil {
		return err
	}
	parent := os.NewFile(uintptr(fds[0]), "agent-parent")
	child := os.NewFile(uintptr(fds[1]), "agent-child")
	unix.CloseOnExec(fds[0])

	flags := bwrap.FlagsFromApplication(d.app, d.paths)
	if flags.Home != "" {
		if err := os.MkdirAll(flags.Home, 0o700); err != nil && !os.IsExist(err) {
			_ = parent.Close()
			_ = child.Close()
			return err
		}
	}
	argv := bwrap.Argv(flags, env, bwrap.InstanceTrailing(d.app, d.paths, d.paths.Agent, 3))
	cmd := exec.Command(argv[0], argv[1:]...)
	cmd.Stdin = nil
	cmd.Stdout = os.Stderr
	cmd.Stderr = os.Stderr
	cmd.ExtraFiles = []*os.File{child}
	if err := cmd.Start(); err != nil {
		_ = parent.Close()
		_ = child.Close()
		return fmt.Errorf("start bwrap: %w", err)
	}
	_ = child.Close()

	fc, err := net.FileConn(parent)
	_ = parent.Close()
	if err != nil {
		_ = cmd.Process.Kill()
		return err
	}
	uc, ok := fc.(*net.UnixConn)
	if !ok {
		_ = fc.Close()
		_ = cmd.Process.Kill()
		return fmt.Errorf("agent socket is not a unix socket")
	}
	d.bwrap = cmd
	d.agent = uc

	go func() {
		err := cmd.Wait()
		d.log.Info("sandbox exited", "error", err)
		d.mu.Lock()
		if d.bwrap == cmd {
			d.bwrap = nil
			if d.agent != nil {
				_ = d.agent.Close()
				d.agent = nil
			}
		}
		d.mu.Unlock()
	}()
	return nil
}

func (d *Daemon) stopSandbox() {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.stopSandboxLocked()
}

func (d *Daemon) stopSandboxLocked() {
	if d.agent != nil {
		_ = d.agent.Close()
		d.agent = nil
	}
	if d.bwrap != nil && d.bwrap.Process != nil {
		_ = d.bwrap.Process.Kill()
		_, _ = d.bwrap.Process.Wait()
		d.bwrap = nil
	}
}

func (d *Daemon) queryWorkloadLocked() bool {
	if d.agent == nil {
		return false
	}
	id := d.nextIDLocked()
	ch := make(chan Message, 1)
	d.waiters[id] = ch
	if err := WriteMsg(d.agent, Message{Type: TypeQueryLiveness, ID: id}, nil); err != nil {
		delete(d.waiters, id)
		return true
	}
	d.mu.Unlock()
	var msg Message
	select {
	case msg = <-ch:
	case <-time.After(FirstReplyTimeout):
		msg.Workload = true
	}
	d.mu.Lock()
	delete(d.waiters, id)
	return msg.Workload
}

func (d *Daemon) readAgent(ctx context.Context) {
	for ctx.Err() == nil {
		conn := d.agentConn()
		if conn == nil {
			select {
			case <-ctx.Done():
				return
			case <-time.After(50 * time.Millisecond):
				continue
			}
		}
		msg, _, err := ReadMsg(conn, 0)
		if err != nil {
			select {
			case <-ctx.Done():
				return
			case <-time.After(50 * time.Millisecond):
				continue
			}
		}
		switch msg.Type {
		case TypeOpenURI:
			d.handleOpenURI(conn, msg)
		default:
			d.mu.Lock()
			ch := d.waiters[msg.ID]
			d.mu.Unlock()
			if ch != nil {
				select {
				case ch <- msg:
				default:
				}
			}
		}
	}
}

func (d *Daemon) handleOpenURI(conn *net.UnixConn, msg Message) {
	err := d.openURI(msg.URI)
	out := Message{Type: TypeOpenURIResult, ID: msg.ID, OK: err == nil}
	if err != nil {
		out.Message = err.Error()
		out.OK = false
	}
	_ = WriteMsg(conn, out, nil)
}

func (d *Daemon) agentConn() *net.UnixConn {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.agent
}

func (d *Daemon) waiter(id string) chan Message {
	ch := make(chan Message, 2)
	d.mu.Lock()
	d.waiters[id] = ch
	d.mu.Unlock()
	return ch
}

func (d *Daemon) unwaiter(id string) {
	d.mu.Lock()
	delete(d.waiters, id)
	d.mu.Unlock()
}

func (d *Daemon) nextIDLocked() string {
	d.seq++
	return strconv.FormatUint(d.seq, 10)
}

func (d *Daemon) hashHex() (string, string) {
	d.mu.Lock()
	defer d.mu.Unlock()
	return d.hashes.Hex()
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

func hostOpenURI(uri string) error {
	if err := CheckHostOpenURI(uri); err != nil {
		return err
	}
	cmd := exec.Command("xdg-open", uri)
	cmd.Stdout = os.Stderr
	cmd.Stderr = os.Stderr
	return cmd.Run()
}
