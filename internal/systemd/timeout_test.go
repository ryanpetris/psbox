package systemd

// Deadline and retry helper tests.

import (
	"context"
	"errors"
	"io"
	"net"
	"testing"
	"time"

	"github.com/godbus/dbus/v5"
)

func TestIsRetryable(t *testing.T) {
	t.Parallel()

	if isRetryable(nil) {
		t.Fatal("nil")
	}
	if isRetryable(context.Canceled) || isRetryable(fmtCanceled()) {
		t.Fatal("canceled must not retry")
	}
	if !isRetryable(context.DeadlineExceeded) {
		t.Fatal("deadline")
	}
	if !isRetryable(io.EOF) || !isRetryable(net.ErrClosed) || !isRetryable(dbus.ErrClosed) {
		t.Fatal("conn closed")
	}
	if !isRetryable(dbus.Error{Name: "org.freedesktop.DBus.Error.NoReply"}) {
		t.Fatal("no reply")
	}
	if isRetryable(errors.New("StartUnit u: job failed")) {
		t.Fatal("job failed must not retry")
	}
}

func fmtCanceled() error {
	return errors.Join(errors.New("StartUnit u"), context.Canceled)
}

func TestRetrySucceedsAfterTimeouts(t *testing.T) {
	t.Parallel()

	u := &User{}
	var n int
	err := u.retry(t.Context(), func(context.Context) error {
		n++
		if n < 3 {
			return context.DeadlineExceeded
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if n != 3 {
		t.Fatalf("attempts %d", n)
	}
}

func TestRetryStopsOnCanceled(t *testing.T) {
	t.Parallel()

	u := &User{}
	err := u.retry(t.Context(), func(context.Context) error {
		return context.Canceled
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("got %v", err)
	}
}

func TestRetryStopsOnPermanentError(t *testing.T) {
	t.Parallel()

	u := &User{}
	want := errors.New("job failed")
	var n int
	err := u.retry(t.Context(), func(context.Context) error {
		n++
		return want
	})
	if !errors.Is(err, want) {
		t.Fatalf("got %v", err)
	}
	if n != 1 {
		t.Fatalf("attempts %d", n)
	}
}

func TestRetryRespectsParentContext(t *testing.T) {
	t.Parallel()

	ctx, cancel := context.WithCancel(t.Context())
	u := &User{}
	var n int
	err := u.retry(ctx, func(context.Context) error {
		n++
		cancel()
		return context.DeadlineExceeded
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("got %v", err)
	}
	if n != 1 {
		t.Fatalf("attempts %d", n)
	}
}

func TestRetryWaitCanBeCanceled(t *testing.T) {
	t.Parallel()

	ctx, cancel := context.WithCancel(t.Context())
	u := &User{}
	err := u.retry(ctx, func(context.Context) error {
		cancel()
		return context.DeadlineExceeded
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("got %v", err)
	}
}

func TestJobTimeoutsAreBounded(t *testing.T) {
	t.Parallel()

	if CallTimeout <= 0 || CallTimeout >= JobTimeout {
		t.Fatalf("call %s job %s", CallTimeout, JobTimeout)
	}
	if ConnectTimeout < CallTimeout {
		t.Fatalf("connect %s", ConnectTimeout)
	}
	if RetryAttempts < 2 {
		t.Fatal("need retries")
	}
	if RetryWait <= 0 || RetryWait > time.Second {
		t.Fatalf("wait %s", RetryWait)
	}
}
