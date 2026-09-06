package instance

// In-sandbox xdg-open listener that forwards http(s) to the host daemon.

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"petris.dev/psbox/internal/config"
)

const (
	xdgOpenSockName    = "psbox-xdg-open.sock"
	maxXDGOpenURIBytes = 64 << 10
)

func xdgOpenSocketPath() string {
	return filepath.Join(config.RuntimeDir(), xdgOpenSockName)
}

func (a *Agent) serveXDGOpen(ctx context.Context, control *net.UnixConn) {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	path := xdgOpenSocketPath()
	_ = os.Remove(path)
	ln, err := net.Listen("unix", path)
	if err != nil {
		a.log.Error("listen for xdg-open", "error", err)
		return
	}
	_ = os.Chmod(path, 0o600)
	var workers sync.WaitGroup
	stop := context.AfterFunc(ctx, func() { _ = ln.Close() })
	defer func() {
		cancel()
		stop()
		_ = ln.Close()
		workers.Wait()
		_ = os.Remove(path)
	}()

	var seq atomic.Uint64
	for {
		conn, err := ln.Accept()
		if err != nil {
			if ctx.Err() != nil {
				return
			}
			a.log.Error("xdg-open accept", "error", err)
			return
		}
		workers.Go(func() {
			stop := context.AfterFunc(ctx, func() { _ = conn.Close() })
			defer stop()
			a.handleXDGOpen(ctx, control, conn, &seq)
		})
	}
}

func (a *Agent) handleXDGOpen(ctx context.Context, control *net.UnixConn, conn net.Conn, seq *atomic.Uint64) {
	defer conn.Close()
	_ = conn.SetDeadline(a.now().Add(FirstReplyTimeout))
	line, err := bufio.NewReader(io.LimitReader(conn, maxXDGOpenURIBytes+1)).ReadString('\n')
	if err != nil {
		return
	}
	uri := strings.TrimSpace(line)
	if !ForwardableURI(uri) {
		_, _ = fmt.Fprintln(conn, "local")
		return
	}
	if err := a.forwardOpenURI(ctx, control, uri, seq); err != nil {
		_, _ = fmt.Fprintln(conn, "error "+err.Error())
		return
	}
	_, _ = fmt.Fprintln(conn, "ok")
}

func (a *Agent) forwardOpenURI(ctx context.Context, control *net.UnixConn, uri string, seq *atomic.Uint64) error {
	id := fmt.Sprintf("uri-%d", seq.Add(1))
	ch := make(chan Message, 1)
	a.mu.Lock()
	a.uriWaiters[id] = ch
	a.mu.Unlock()
	defer func() {
		a.mu.Lock()
		delete(a.uriWaiters, id)
		a.mu.Unlock()
	}()
	if err := WriteMsg(control, Message{Type: TypeOpenURI, ID: id, URI: uri}, nil); err != nil {
		return err
	}
	select {
	case msg := <-ch:
		if !msg.OK {
			return fmt.Errorf("%s", msg.Message)
		}
		return nil
	case <-ctx.Done():
		return ctx.Err()
	case <-time.After(FirstReplyTimeout):
		return fmt.Errorf("open_uri timeout")
	}
}
