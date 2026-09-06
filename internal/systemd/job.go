package systemd

// StartUnit/StopUnit jobs and JobRemoved.

import (
	"context"
	"fmt"

	"github.com/godbus/dbus/v5"
)

func (u *User) runJob(ctx context.Context, method, unit string) error {
	ctx, cancel := context.WithTimeout(ctx, JobTimeout)
	defer cancel()

	conn, err := u.connection(ctx)
	if err != nil {
		return err
	}

	sigc := make(chan *dbus.Signal, 16)
	conn.Signal(sigc)
	defer conn.RemoveSignal(sigc)

	var job dbus.ObjectPath
	if err := u.managerCall(ctx, CallTimeout, method, []any{unit, mode}, &job); err != nil {
		return fmt.Errorf("%s %s: %w", method, unit, err)
	}
	return waitJobRemoved(ctx, sigc, job, method, unit)
}

func waitJobRemoved(ctx context.Context, sigc <-chan *dbus.Signal, job dbus.ObjectPath, method, unit string) error {
	for {
		select {
		case <-ctx.Done():
			return fmt.Errorf("%s %s: %w", method, unit, ctx.Err())
		case sig, ok := <-sigc:
			if !ok {
				return fmt.Errorf("%s %s: %w", method, unit, dbus.ErrClosed)
			}
			if sig.Name != jobSignal {
				continue
			}
			removed, result, ok := parseJobRemoved(sig.Body, job)
			if !ok || !removed {
				continue
			}
			if result != "done" {
				return fmt.Errorf("%s %s: job %s", method, unit, result)
			}
			return nil
		}
	}
}

func parseJobRemoved(body []any, want dbus.ObjectPath) (matched bool, result string, ok bool) {
	if len(body) < 4 {
		return false, "", false
	}
	path, ok := body[1].(dbus.ObjectPath)
	if !ok {
		return false, "", false
	}
	if path != want {
		return false, "", true
	}
	result, ok = body[3].(string)
	if !ok {
		return false, "", false
	}
	return true, result, true
}
