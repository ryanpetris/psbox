package systemd

// Session-bus connection to the user manager, with a private-socket fallback.

import (
	"context"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/godbus/dbus/v5"
)

func (u *User) connection(ctx context.Context) (*dbus.Conn, error) {
	u.mu.Lock()
	defer u.mu.Unlock()
	if u.conn != nil {
		return u.conn, nil
	}

	ctx, cancel := context.WithTimeout(ctx, ConnectTimeout)
	defer cancel()
	if time.Until(ctxDeadline(ctx)) <= 0 {
		return nil, fmt.Errorf("connect to systemd user manager: %w", ctx.Err())
	}

	if addr := sessionBusAddress(u.runtimeDir); addr != "" {
		conn, err := u.dialAndAuth(ctx, addr)
		if err == nil {
			if err = u.hello(ctx, conn); err != nil {
				_ = conn.Close()
			} else if err = u.subscribeJobs(ctx, conn, true); err != nil {
				_ = conn.Close()
			} else {
				u.conn = conn
				u.onBus = true
				return conn, nil
			}
		}
	}

	conn, err := u.dialPrivate(ctx)
	if err != nil {
		return nil, err
	}
	if err := u.subscribeJobs(ctx, conn, false); err != nil {
		_ = conn.Close()
		return nil, err
	}
	u.conn = conn
	u.onBus = false
	return conn, nil
}

func (u *User) dialPrivate(ctx context.Context) (*dbus.Conn, error) {
	path := filepath.Join(u.runtimeDir, "systemd", "private")
	timeout := time.Until(ctxDeadline(ctx))
	if timeout <= 0 {
		return nil, fmt.Errorf("connect to systemd user manager: %w", ctx.Err())
	}
	raw, err := net.DialTimeout("unix", path, timeout)
	if err != nil {
		return nil, fmt.Errorf("connect to systemd user manager: %w", err)
	}
	_ = raw.SetDeadline(ctxDeadline(ctx))
	// The setup context must not be the Conn lifetime. WithContext
	// cancels the connection when this function returns.
	conn, err := dbus.NewConn(raw)
	if err != nil {
		_ = raw.Close()
		return nil, fmt.Errorf("connect to systemd user manager: %w", err)
	}
	if err := u.auth(conn); err != nil {
		_ = conn.Close()
		return nil, err
	}
	_ = raw.SetDeadline(time.Time{})
	return conn, nil
}

func (u *User) dialAndAuth(ctx context.Context, addr string) (*dbus.Conn, error) {
	if path, ok := unixPathAddr(addr); ok {
		timeout := time.Until(ctxDeadline(ctx))
		if timeout <= 0 {
			return nil, fmt.Errorf("connect to session bus: %w", ctx.Err())
		}
		raw, err := net.DialTimeout("unix", path, timeout)
		if err != nil {
			return nil, fmt.Errorf("connect to session bus: %w", err)
		}
		_ = raw.SetDeadline(ctxDeadline(ctx))
		conn, err := dbus.NewConn(raw)
		if err != nil {
			_ = raw.Close()
			return nil, fmt.Errorf("connect to session bus: %w", err)
		}
		if err := u.auth(conn); err != nil {
			_ = conn.Close()
			return nil, err
		}
		_ = raw.SetDeadline(time.Time{})
		return conn, nil
	}

	conn, err := dbus.Dial(addr)
	if err != nil {
		return nil, fmt.Errorf("connect to session bus: %w", err)
	}
	if err := u.auth(conn); err != nil {
		_ = conn.Close()
		return nil, err
	}
	return conn, nil
}

func (u *User) auth(conn *dbus.Conn) error {
	if err := conn.Auth([]dbus.Auth{dbus.AuthExternal(strconv.Itoa(u.uid))}); err != nil {
		return fmt.Errorf("authenticate to systemd user manager: %w", err)
	}
	return nil
}

func (u *User) hello(ctx context.Context, conn *dbus.Conn) error {
	var unique string
	if err := conn.BusObject().CallWithContext(ctx, "org.freedesktop.DBus.Hello", 0).Store(&unique); err != nil {
		return fmt.Errorf("session bus hello: %w", err)
	}
	return nil
}

func (u *User) subscribeJobs(ctx context.Context, conn *dbus.Conn, onBus bool) error {
	if err := conn.Object(dest, objPath).CallWithContext(ctx, iface+".Subscribe", 0).Err; err != nil {
		return fmt.Errorf("subscribe to systemd jobs: %w", err)
	}
	if !onBus {
		return nil
	}
	if err := conn.AddMatchSignalContext(ctx,
		dbus.WithMatchInterface(iface),
		dbus.WithMatchMember("JobRemoved"),
	); err != nil {
		return fmt.Errorf("match JobRemoved: %w", err)
	}
	return nil
}

func (u *User) dropConn() {
	u.mu.Lock()
	defer u.mu.Unlock()
	if u.conn == nil {
		return
	}
	_ = u.conn.Close()
	u.conn = nil
	u.onBus = false
}

func (u *User) managerCall(ctx context.Context, timeout time.Duration, method string, args []any, out any) error {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	conn, err := u.connection(ctx)
	if err != nil {
		return err
	}
	call := conn.Object(dest, objPath).CallWithContext(ctx, iface+"."+method, 0, args...)
	if out != nil {
		return call.Store(out)
	}
	return call.Err
}

func sessionBusAddress(runtimeDir string) string {
	if v := os.Getenv("DBUS_SESSION_BUS_ADDRESS"); v != "" {
		return v
	}
	if runtimeDir == "" {
		return ""
	}
	return "unix:path=" + filepath.Join(runtimeDir, "bus")
}

func unixPathAddr(addr string) (string, bool) {
	rest, ok := strings.CutPrefix(addr, "unix:")
	if !ok {
		return "", false
	}
	for _, part := range strings.Split(rest, ",") {
		key, val, found := strings.Cut(part, "=")
		if found && key == "path" && val != "" {
			return val, true
		}
	}
	return "", false
}

func ctxDeadline(ctx context.Context) time.Time {
	deadline, ok := ctx.Deadline()
	if !ok {
		return time.Now().Add(ConnectTimeout)
	}
	return deadline
}
