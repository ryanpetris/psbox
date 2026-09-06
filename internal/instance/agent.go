package instance

// In-sandbox agent: spawn, reap, private bus, idle exit.

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"golang.org/x/sys/unix"

	"petris.dev/psbox/internal/bwrap"
	"petris.dev/psbox/internal/config"
)

// Agent is psboxa, PID 2 inside bwrap.
type Agent struct {
	log       *slog.Logger
	procDir   string
	idleAfter time.Duration
	now       func() time.Time

	mu         sync.Mutex
	handlerMu  sync.Mutex
	spawns     map[string]*spawnSession
	live       int
	infra      []InfraRoot
	infraSeen  []InfraRoot
	busAddr    string
	busProc    *os.Process
	busDone    chan error
	workers    sync.WaitGroup
	lastBusy   time.Time
	uriWaiters map[string]chan Message
}

type spawnSession struct {
	id      string
	proc    *os.Process
	pgid    int
	started bool
}

// NewAgent returns an in-sandbox agent.
func NewAgent(log *slog.Logger) *Agent {
	if log == nil {
		log = slog.Default()
	}
	return &Agent{
		log:        log,
		procDir:    "/proc",
		idleAfter:  DefaultIdleAfter,
		now:        time.Now,
		spawns:     map[string]*spawnSession{},
		uriWaiters: map[string]chan Message{},
	}
}

// Run serves the daemon control socket until idle exit or error.
func (a *Agent) Run(ctx context.Context) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	fdStr := os.Getenv(config.ControlFDEnv)
	if fdStr == "" {
		return fmt.Errorf("psboxa requires a daemon control socket")
	}
	fd, err := strconv.Atoi(fdStr)
	if err != nil || fd < 0 {
		return fmt.Errorf("invalid %s", config.ControlFDEnv)
	}
	file := os.NewFile(uintptr(fd), "control")
	if file == nil {
		return fmt.Errorf("psboxa requires a daemon control socket")
	}
	conn, err := net.FileConn(file)
	_ = file.Close()
	if err != nil {
		return fmt.Errorf("control socket: %w", err)
	}
	uc, ok := conn.(*net.UnixConn)
	if !ok {
		_ = conn.Close()
		return fmt.Errorf("control socket is not a unix socket")
	}
	defer func() {
		cancel()
		_ = uc.Close()
		a.shutdown()
		a.workers.Wait()
	}()

	a.lastBusy = a.now()
	if os.Getenv(config.DBusEnv) == config.DBusPrivate {
		if err := a.startPrivateBus(ctx); err != nil {
			return err
		}
	}
	if hostURLsEnabled() {
		a.workers.Go(func() { a.serveXDGOpen(ctx, uc) })
	}

	idle := time.NewTicker(time.Second)
	defer idle.Stop()

	msgc := make(chan readResult)
	a.workers.Go(func() {
		for {
			msg, files, err := ReadMsg(uc, 3)
			select {
			case msgc <- readResult{msg: msg, files: files, err: err}:
			case <-ctx.Done():
				closeFiles(files)
				return
			}
			if err != nil {
				return
			}
		}
	})

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case err := <-a.busDone:
			if ctx.Err() != nil {
				return ctx.Err()
			}
			if err != nil {
				return fmt.Errorf("private dbus-daemon exited: %w", err)
			}
			return fmt.Errorf("private dbus-daemon exited unexpectedly")
		case <-idle.C:
			if a.shouldExit() {
				return nil
			}
		case res := <-msgc:
			if res.err != nil {
				return res.err
			}
			if err := a.handle(ctx, uc, res.msg, res.files); err != nil {
				a.log.Error("agent request failed", "type", res.msg.Type, "error", err)
			}
		}
	}
}

type readResult struct {
	msg   Message
	files []*os.File
	err   error
}

func (a *Agent) handle(ctx context.Context, conn *net.UnixConn, msg Message, files []*os.File) error {
	switch msg.Type {
	case TypeSpawn:
		return a.spawn(conn, msg, files)
	case TypeSignal:
		closeFiles(files)
		return a.signal(msg)
	case TypeQueryLiveness:
		closeFiles(files)
		return WriteMsg(conn, Message{Type: TypeLiveness, ID: msg.ID, Workload: a.workload()}, nil)
	case TypeOpenURIResult:
		closeFiles(files)
		a.mu.Lock()
		ch := a.uriWaiters[msg.ID]
		a.mu.Unlock()
		if ch != nil {
			select {
			case ch <- msg:
			default:
			}
		}
		return nil
	default:
		closeFiles(files)
		return fmt.Errorf("unknown message %q", msg.Type)
	}
}

func (a *Agent) spawn(conn *net.UnixConn, msg Message, files []*os.File) error {
	defer closeFiles(files)
	if len(msg.Argv) == 0 {
		return WriteMsg(conn, Message{Type: TypeSpawnError, ID: msg.ID, Message: "empty argv"}, nil)
	}
	stdin, stdout, stderr := os.Stdin, os.Stdout, os.Stderr
	if len(files) >= 3 {
		stdin, stdout, stderr = files[0], files[1], files[2]
	}

	env := spawnEnv(msg.Env, a.busAddr)
	a.handlerMu.Lock()
	if hostURLsEnabled() {
		if err := ensureHostURLHandler(env); err != nil {
			a.log.Error("install host url handler", "error", err)
		}
	} else if err := removeHostURLHandler(env); err != nil {
		a.log.Error("remove host url handler", "error", err)
	}
	a.handlerMu.Unlock()

	cwd := msg.Cwd
	if cwd == "" {
		cwd = "/"
	}
	if st, err := os.Stat(cwd); err != nil || !st.IsDir() {
		cwd = "/"
	}

	cmd := msg.Argv[0]
	path, err := lookPath(cmd, env, cwd)
	var proc *os.Process
	if err == nil {
		proc, err = os.StartProcess(path, msg.Argv, &os.ProcAttr{
			Dir:   cwd,
			Env:   mapEnv(env),
			Files: []*os.File{stdin, stdout, stderr},
			Sys:   &syscall.SysProcAttr{Setpgid: true},
		})
	}
	if err != nil {
		return WriteMsg(conn, Message{
			Type:    TypeSpawnError,
			ID:      msg.ID,
			Errno:   errnoOf(err),
			Message: err.Error(),
		}, nil)
	}

	sess := &spawnSession{id: msg.ID, proc: proc, pgid: proc.Pid, started: true}
	a.mu.Lock()
	a.spawns[msg.ID] = sess
	a.live++
	a.lastBusy = a.now()
	a.mu.Unlock()
	a.log.Info("command started", "command", cmd, "pid", proc.Pid)

	ackErr := WriteMsg(conn, Message{Type: TypeSpawned, ID: msg.ID, PID: proc.Pid}, nil)
	if ackErr != nil {
		_ = syscall.Kill(-proc.Pid, syscall.SIGKILL)
		_ = proc.Kill()
	}
	a.workers.Go(func() {
		state, err := proc.Wait()
		code, sig := waitStatus(state, err)
		a.mu.Lock()
		delete(a.spawns, msg.ID)
		a.live--
		a.lastBusy = a.now()
		a.mu.Unlock()
		a.log.Info("command exited", "command", cmd, "pid", proc.Pid, "code", code, "signal", sig)
		_ = WriteMsg(conn, Message{Type: TypeExited, ID: msg.ID, Code: intPtr(code), Signal: sigPtr(sig)}, nil)
	})
	return ackErr
}

func (a *Agent) signal(msg Message) error {
	a.mu.Lock()
	sess := a.spawns[msg.ID]
	a.mu.Unlock()
	if sess == nil || sess.proc == nil {
		return nil
	}
	pgid := sess.pgid
	if pgid <= 0 {
		pgid = sess.proc.Pid
	}
	return syscall.Kill(-pgid, syscall.Signal(msg.Signum))
}

func (a *Agent) workload() bool {
	procs, err := ScanProc(a.procDir)
	if err != nil {
		a.log.Error("scan /proc", "error", err)
		return true
	}
	a.mu.Lock()
	roots := append([]InfraRoot(nil), a.infra...)
	seen := append([]InfraRoot(nil), a.infraSeen...)
	live := a.live
	procDir := a.procDir
	a.mu.Unlock()
	if live > 0 {
		return true
	}

	seeds := append(append([]InfraRoot(nil), roots...), seen...)
	infra := expandInfra(os.Getpid(), 1, seeds, procs, func(p Proc) bool {
		return hasDBusStarter(procDir, p.PID)
	})
	a.mu.Lock()
	a.infraSeen = rememberInfra(infra, procs)
	a.mu.Unlock()
	return hasWorkload(os.Getpid(), 1, infra, procs)
}

func (a *Agent) shouldExit() bool {
	if a.workload() {
		a.mu.Lock()
		a.lastBusy = a.now()
		a.mu.Unlock()
		return false
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.now().Sub(a.lastBusy) >= a.idleAfter
}

func (a *Agent) shutdown() {
	a.mu.Lock()
	proc := a.busProc
	spawns := make([]*spawnSession, 0, len(a.spawns))
	for _, s := range a.spawns {
		spawns = append(spawns, s)
	}
	a.mu.Unlock()
	for _, s := range spawns {
		if s.proc != nil {
			_ = syscall.Kill(-s.pgid, syscall.SIGKILL)
			_ = s.proc.Kill()
		}
	}
	if proc != nil {
		_ = proc.Kill()
	}
}

func spawnEnv(client map[string]string, busAddr string) map[string]string {
	env := map[string]string{}
	for k, v := range client {
		env[k] = v
	}
	delete(env, "WAYLAND_DISPLAY")
	delete(env, "XAUTHORITY")
	if v := os.Getenv("WAYLAND_DISPLAY"); v != "" {
		env["WAYLAND_DISPLAY"] = v
	}
	if v := os.Getenv("XAUTHORITY"); v != "" {
		env["XAUTHORITY"] = v
	}
	runtime := os.Getenv("XDG_RUNTIME_DIR")
	if runtime == "" {
		runtime = config.DefaultRuntimeDir()
	}
	env["XDG_RUNTIME_DIR"] = runtime
	path := env["PATH"]
	if path == "" {
		path = config.DefaultPATH
	}
	if hostURLsEnabled() {
		if dir := bwrap.XDGOpenBinDir(runtime); dir != "" {
			path = prependPATH(path, dir)
		}
	}
	env["PATH"] = path
	if busAddr != "" {
		env["DBUS_SESSION_BUS_ADDRESS"] = busAddr
		delete(env, "DBUS_SESSION_BUS_PID")
		delete(env, "DBUS_SESSION_BUS_WINDOWID")
	}
	return env
}

func hostURLsEnabled() bool {
	return os.Getenv(config.HostURLsEnv) != config.HostURLsOff
}

func prependPATH(path, dir string) string {
	if dir == "" {
		return path
	}
	if path == "" {
		return dir
	}
	return dir + ":" + path
}

func mapEnv(env map[string]string) []string {
	out := make([]string, 0, len(env))
	for k, v := range env {
		out = append(out, k+"="+v)
	}
	return out
}

func lookPath(name string, env map[string]string, cwd string) (string, error) {
	check := func(path string) (string, error) {
		if !filepath.IsAbs(path) {
			path = filepath.Join(cwd, path)
		}
		st, err := os.Stat(path)
		if err != nil {
			return "", err
		}
		if st.IsDir() {
			return "", &os.PathError{Op: "exec", Path: path, Err: syscall.EACCES}
		}
		if err := unix.Access(path, unix.X_OK); err != nil {
			return "", &os.PathError{Op: "exec", Path: path, Err: err}
		}
		return path, nil
	}
	if strings.Contains(name, "/") {
		return check(name)
	}
	var denied error
	for _, dir := range filepath.SplitList(env["PATH"]) {
		path, err := check(filepath.Join(dir, name))
		if err == nil {
			return path, nil
		}
		if errors.Is(err, syscall.EACCES) {
			denied = err
		}
	}
	if denied != nil {
		return "", denied
	}
	return "", &os.PathError{Op: "look up executable", Path: name, Err: syscall.ENOENT}
}

func waitStatus(st *os.ProcessState, err error) (code int, sig int) {
	if err != nil && st == nil {
		return 1, 0
	}
	ws, ok := st.Sys().(syscall.WaitStatus)
	if !ok {
		return st.ExitCode(), 0
	}
	if ws.Signaled() {
		return 0, int(ws.Signal())
	}
	return ws.ExitStatus(), 0
}

func errnoOf(err error) int {
	if err == nil {
		return 0
	}
	var errno syscall.Errno
	if errors.As(err, &errno) {
		return int(errno)
	}
	return int(syscall.EIO)
}

func intPtr(v int) *int { return &v }

func sigPtr(v int) *int {
	if v == 0 {
		return nil
	}
	return &v
}
