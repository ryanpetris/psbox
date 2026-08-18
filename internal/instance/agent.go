package instance

// In-sandbox agent: spawn, reap, private bus, idle exit.

import (
	"context"
	"fmt"
	"log/slog"
	"net"
	"os"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

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
	busAddr    string
	busProc    *os.Process
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
	fdStr := os.Getenv(ControlFDEnv)
	if fdStr == "" {
		return fmt.Errorf("psboxa requires a daemon control socket")
	}
	fd, err := strconv.Atoi(fdStr)
	if err != nil || fd < 0 {
		return fmt.Errorf("invalid %s", ControlFDEnv)
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
	defer uc.Close()

	a.lastBusy = a.now()
	if os.Getenv(DBusEnv) == config.DBusPrivate {
		if err := a.startPrivateBus(ctx); err != nil {
			return err
		}
	}
	if hostURLsEnabled() {
		go a.serveXDGOpen(ctx, uc)
	}

	idle := time.NewTicker(time.Second)
	defer idle.Stop()

	msgc := make(chan readResult, 1)
	go func() {
		for {
			msg, files, err := ReadMsg(uc, 8)
			msgc <- readResult{msg: msg, files: files, err: err}
			if err != nil {
				return
			}
		}
	}()

	for {
		select {
		case <-ctx.Done():
			a.shutdown()
			return ctx.Err()
		case <-idle.C:
			if a.shouldExit() {
				a.shutdown()
				return nil
			}
		case res := <-msgc:
			if res.err != nil {
				a.shutdown()
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

	proc, err := os.StartProcess(lookPath(msg.Argv[0], env), msg.Argv, &os.ProcAttr{
		Dir:   cwd,
		Env:   mapEnv(env),
		Files: []*os.File{stdin, stdout, stderr},
		Sys:   &syscall.SysProcAttr{Setpgid: true},
	})
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

	if err := WriteMsg(conn, Message{Type: TypeSpawned, ID: msg.ID, PID: proc.Pid}, nil); err != nil {
		return err
	}

	go func() {
		state, err := proc.Wait()
		code, sig := waitStatus(state, err)
		a.mu.Lock()
		delete(a.spawns, msg.ID)
		a.live--
		a.lastBusy = a.now()
		a.mu.Unlock()
		_ = WriteMsg(conn, Message{Type: TypeExited, ID: msg.ID, Code: intPtr(code), Signal: sigPtr(sig)}, nil)
	}()
	return nil
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
	live := a.live
	a.mu.Unlock()
	if live > 0 {
		return true
	}
	return WorkloadPresent(os.Getpid(), 1, roots, procs)
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
			_ = syscall.Kill(-s.pgid, syscall.SIGHUP)
		}
	}
	if proc != nil {
		_ = proc.Signal(syscall.SIGTERM)
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
	return os.Getenv(HostURLsEnv) != HostURLsOff
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

func lookPath(name string, env map[string]string) string {
	if strings.Contains(name, "/") {
		return name
	}
	path := env["PATH"]
	if path == "" {
		path = os.Getenv("PATH")
	}
	if path == "" {
		path = config.DefaultPATH
	}
	for _, dir := range strings.Split(path, ":") {
		if dir == "" {
			continue
		}
		cand := dir + "/" + name
		if st, err := os.Stat(cand); err == nil && !st.IsDir() {
			return cand
		}
	}
	return name
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
	if errno, ok := err.(syscall.Errno); ok {
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
