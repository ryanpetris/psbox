package systemd

// Deadlines and retries for user-manager D-Bus calls.

import (
	"context"
	"errors"
	"io"
	"net"
	"time"

	"github.com/godbus/dbus/v5"
)

const (
	// CallTimeout is one Manager or Properties method call.
	CallTimeout = 2 * time.Second
	// JobTimeout is StartUnit/StopUnit through JobRemoved.
	JobTimeout = 5 * time.Second
	// ConnectTimeout is dial, AUTH, and Subscribe.
	ConnectTimeout = 5 * time.Second
	// RetryAttempts is how many times an idempotent call is tried.
	RetryAttempts = 3
	// RetryWait is the pause between idempotent attempts.
	RetryWait = 100 * time.Millisecond
)

func (u *User) retry(ctx context.Context, fn func(context.Context) error) error {
	var last error
	for attempt := 0; attempt < RetryAttempts; attempt++ {
		if err := ctx.Err(); err != nil {
			return err
		}
		last = fn(ctx)
		if last == nil {
			return nil
		}
		if !isRetryable(last) {
			return last
		}
		u.dropConn()
		if attempt == RetryAttempts-1 {
			break
		}
		timer := time.NewTimer(RetryWait)
		select {
		case <-ctx.Done():
			timer.Stop()
			return ctx.Err()
		case <-timer.C:
		}
	}
	return last
}

func isRetryable(err error) bool {
	if err == nil {
		return false
	}
	if errors.Is(err, context.Canceled) {
		return false
	}
	if errors.Is(err, context.DeadlineExceeded) {
		return true
	}
	if errors.Is(err, io.EOF) || errors.Is(err, io.ErrClosedPipe) || errors.Is(err, net.ErrClosed) || errors.Is(err, dbus.ErrClosed) {
		return true
	}
	var ne net.Error
	if errors.As(err, &ne) && ne.Timeout() {
		return true
	}
	var de dbus.Error
	if errors.As(err, &de) {
		switch de.Name {
		case "org.freedesktop.DBus.Error.NoReply",
			"org.freedesktop.DBus.Error.Timeout",
			"org.freedesktop.DBus.Error.Disconnected",
			"org.freedesktop.DBus.Error.NoServer",
			"org.freedesktop.DBus.Error.IOError":
			return true
		}
	}
	return false
}
