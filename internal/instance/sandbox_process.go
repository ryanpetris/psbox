package instance

// One sandbox process owns its control reader, requests, and host URL workers.

import (
	"context"
	"fmt"
	"net"
	"os"
	"os/exec"
	"strconv"
	"sync"
	"time"

	"golang.org/x/sys/unix"

	"petris.dev/psbox/internal/bwrap"
	"petris.dev/psbox/internal/config"
)

const maxHostURIOpens = 4

// sandboxProcess keeps replies and shutdown confined to one sandbox lifetime.
type sandboxProcess struct {
	conn       *net.UnixConn
	hashes     bwrap.Hashes
	cancel     context.CancelFunc
	done       chan struct{}
	readerDone chan struct{}
	mu         sync.Mutex
	seq        uint64
	closed     bool
	waiters    map[string]chan Message
}

func (d *Daemon) startSandbox(ctx context.Context, app *config.Application, paths config.Paths, env bwrap.Env, hashes bwrap.Hashes) (*sandboxProcess, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	fds, err := unix.Socketpair(unix.AF_UNIX, unix.SOCK_SEQPACKET|unix.SOCK_CLOEXEC, 0)
	if err != nil {
		return nil, err
	}
	parent := os.NewFile(uintptr(fds[0]), "agent-parent")
	child := os.NewFile(uintptr(fds[1]), "agent-child")
	defer child.Close()
	fc, err := net.FileConn(parent)
	_ = parent.Close()
	if err != nil {
		return nil, err
	}
	uc, ok := fc.(*net.UnixConn)
	if !ok {
		_ = fc.Close()
		return nil, fmt.Errorf("agent socket is not a unix socket")
	}
	started := false
	defer func() {
		if !started {
			_ = uc.Close()
		}
	}()
	flags := bwrap.FlagsFromApplication(app, paths)
	if flags.Home != "" {
		if err := os.MkdirAll(flags.Home, 0o700); err != nil {
			return nil, err
		}
	}
	argv := bwrap.Argv(flags, env, bwrap.InstanceTrailing(app, paths, paths.Agent, 3))
	lifetime, cancel := context.WithCancel(ctx)
	cmd := d.command(lifetime, argv[0], argv[1:]...)
	cmd.Stdout, cmd.Stderr = os.Stderr, os.Stderr
	cmd.ExtraFiles = []*os.File{child}
	if err := cmd.Start(); err != nil {
		cancel()
		return nil, fmt.Errorf("start bwrap: %w", err)
	}
	run := &sandboxProcess{conn: uc, hashes: hashes, cancel: cancel, done: make(chan struct{}), readerDone: make(chan struct{}), waiters: map[string]chan Message{}}
	started = true
	go func() {
		defer close(run.done)
		err := cmd.Wait()
		d.log.Info("sandbox exited", "error", err)
		cancel()
		_ = uc.Close()
	}()
	go run.readAgent(lifetime, d.openURI)
	return run, nil
}

func (s *sandboxProcess) request(msg Message, files []*os.File) (string, chan Message, error) {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return "", nil, fmt.Errorf("sandbox agent disconnected")
	}
	s.seq++
	id := strconv.FormatUint(s.seq, 10)
	ch := make(chan Message, 2)
	s.waiters[id] = ch
	s.mu.Unlock()
	msg.ID = id
	if err := WriteMsg(s.conn, msg, files); err != nil {
		s.unwaiter(id)
		return "", nil, err
	}
	return id, ch, nil
}

func (s *sandboxProcess) unwaiter(id string) {
	s.mu.Lock()
	delete(s.waiters, id)
	s.mu.Unlock()
}

func (s *sandboxProcess) finished() bool {
	select {
	case <-s.readerDone:
		return true
	default:
		return false
	}
}

func (s *sandboxProcess) stop() {
	s.cancel()
	_ = s.conn.Close()
	<-s.done
	<-s.readerDone
}

func (s *sandboxProcess) workload(ctx context.Context) bool {
	id, ch, err := s.request(Message{Type: TypeQueryLiveness}, nil)
	if err != nil {
		return !s.finished()
	}
	defer s.unwaiter(id)
	select {
	case msg, ok := <-ch:
		return ok && (msg.Type != TypeLiveness || msg.Workload)
	case <-ctx.Done():
		return true
	case <-time.After(FirstReplyTimeout):
		return true
	}
}

func (s *sandboxProcess) readAgent(ctx context.Context, openURI func(context.Context, string) error) {
	ctx, cancel := context.WithCancel(ctx)
	var workers sync.WaitGroup
	slots := make(chan struct{}, maxHostURIOpens)
	defer close(s.readerDone)
	defer func() {
		cancel()
		s.cancel()
		_ = s.conn.Close()
		s.mu.Lock()
		s.closed = true
		for id, ch := range s.waiters {
			close(ch)
			delete(s.waiters, id)
		}
		s.mu.Unlock()
		workers.Wait()
	}()
	for {
		msg, _, err := ReadMsg(s.conn, 0)
		if err != nil {
			return
		}
		if msg.Type == TypeOpenURI {
			select {
			case slots <- struct{}{}:
				workers.Go(func() {
					defer func() { <-slots }()
					ctx, cancel := context.WithTimeout(ctx, FirstReplyTimeout)
					defer cancel()
					err := CheckHostOpenURI(msg.URI)
					if err == nil {
						err = openURI(ctx, msg.URI)
					}
					out := Message{Type: TypeOpenURIResult, ID: msg.ID, OK: err == nil}
					if err != nil {
						out.Message = err.Error()
					}
					_ = WriteMsg(s.conn, out, nil)
				})
			default:
				// A saturated opener does not accumulate goroutines or queued requests.
				if err := WriteMsg(s.conn, Message{Type: TypeOpenURIResult, ID: msg.ID, Message: "too many pending URL opens"}, nil); err != nil {
					return
				}
			}
			continue
		}
		s.mu.Lock()
		if ch := s.waiters[msg.ID]; ch != nil {
			select {
			case ch <- msg:
			default:
			}
		}
		s.mu.Unlock()
	}
}

func hostOpenURI(ctx context.Context, uri string) error {
	if err := CheckHostOpenURI(uri); err != nil {
		return err
	}
	cmd := exec.CommandContext(ctx, "xdg-open", uri)
	cmd.Stdout, cmd.Stderr = os.Stderr, os.Stderr
	return cmd.Run()
}
