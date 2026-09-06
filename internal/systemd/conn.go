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
	conn, err := u.dialAndAuth(ctx, "unix:path="+dbus.EscapeBusAddressValue(path))
	if err != nil {
		return nil, fmt.Errorf("connect to systemd user manager: %w", err)
	}
	return conn, nil
}

func (u *User) dialAndAuth(ctx context.Context, addr string) (*dbus.Conn, error) {
	ctx, cancel := context.WithTimeout(ctx, ConnectTimeout)
	defer cancel()
	var last error
	for _, candidate := range strings.Split(addr, ";") {
		endpoint, err := parseBusAddress(candidate)
		if err != nil {
			last = err
			continue
		}
		conn, err := u.connectBus(ctx, endpoint)
		if err == nil {
			return conn, nil
		}
		last = err
		if ctx.Err() != nil {
			return nil, ctx.Err()
		}
	}
	return nil, fmt.Errorf("connect to session bus: %w", last)
}

func (u *User) connectBus(ctx context.Context, endpoint busAddress) (*dbus.Conn, error) {
	raw, err := (&net.Dialer{}).DialContext(ctx, endpoint.network, endpoint.address)
	if err != nil {
		return nil, err
	}
	accepted := false
	defer func() {
		if !accepted {
			_ = raw.Close()
		}
	}()
	if err := raw.SetDeadline(ctxDeadline(ctx)); err != nil {
		return nil, err
	}
	canceled := make(chan struct{})
	stop := context.AfterFunc(ctx, func() { _ = raw.Close(); close(canceled) })
	disarmed := false
	defer func() {
		if !disarmed && !stop() {
			<-canceled
		}
	}()
	if endpoint.noncefile != "" {
		nonce, err := readBusNonce(endpoint.noncefile)
		if err != nil {
			return nil, err
		}
		if _, err := raw.Write(nonce); err != nil {
			return nil, err
		}
	}
	// Authentication uses the setup deadline, while the established connection
	// owns its lifetime independently of the caller's setup context.
	conn, err := dbus.NewConn(raw)
	if err != nil {
		return nil, err
	}
	if err := u.auth(conn); err != nil {
		_ = conn.Close()
		return nil, err
	}
	disarmed = true
	if !stop() {
		<-canceled
		_ = conn.Close()
		return nil, ctx.Err()
	}
	if err := ctx.Err(); err != nil {
		_ = conn.Close()
		return nil, err
	}
	if err := raw.SetDeadline(time.Time{}); err != nil {
		_ = conn.Close()
		return nil, err
	}
	accepted = true
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

func ctxDeadline(ctx context.Context) time.Time {
	deadline, ok := ctx.Deadline()
	if !ok {
		return time.Now().Add(ConnectTimeout)
	}
	return deadline
}
