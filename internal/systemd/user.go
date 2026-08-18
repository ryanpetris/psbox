package systemd

// User-manager D-Bus client (same socket systemctl --user uses).

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"

	"github.com/godbus/dbus/v5"

	"petris.dev/psbox/internal/config"
)

const (
	dest      = "org.freedesktop.systemd1"
	objPath   = "/org/freedesktop/systemd1"
	iface     = "org.freedesktop.systemd1.Manager"
	mode      = "replace"
	jobSignal = iface + ".JobRemoved"
)

// Unit is one systemd unit from ListUnitsByPatterns plus selected properties.
type Unit struct {
	Name                 string
	ActiveState          string
	SubState             string
	Path                 string
	MainPID              uint32
	ActiveEnterTimestamp uint64
	NRestarts            uint32
	NAccepted            uint32
}

// Control starts, stops, and lists user units.
type Control interface {
	Start(ctx context.Context, unit string) error
	Stop(ctx context.Context, unit string) error
	List(ctx context.Context, patterns []string) ([]Unit, error)
}

// User is a lazy connection to the systemd user manager.
type User struct {
	runtimeDir string
	uid        int

	mu   sync.Mutex
	conn *dbus.Conn
}

var _ Control = (*User)(nil)

// NewUser returns a user-manager client using $XDG_RUNTIME_DIR
// (or /run/user/<uid> when that is unset).
func NewUser() *User {
	return &User{
		runtimeDir: config.RuntimeDir(),
		uid:        os.Getuid(),
	}
}

// Start starts unit and waits for the systemd job to finish.
func (u *User) Start(ctx context.Context, unit string) error {
	return u.runJob(ctx, "StartUnit", unit)
}

// Stop stops unit and waits for the systemd job to finish.
func (u *User) Stop(ctx context.Context, unit string) error {
	return u.runJob(ctx, "StopUnit", unit)
}

// List returns units matching patterns (for example psboxd@*.socket).
func (u *User) List(ctx context.Context, patterns []string) ([]Unit, error) {
	conn, err := u.connection()
	if err != nil {
		return nil, err
	}
	var rows []listRow
	err = conn.Object(dest, objPath).CallWithContext(ctx, iface+".ListUnitsByPatterns", 0, []string{}, patterns).Store(&rows)
	if err != nil {
		return nil, fmt.Errorf("list units: %w", err)
	}
	out := make([]Unit, 0, len(rows))
	for _, row := range rows {
		u := Unit{
			Name:        row.Name,
			ActiveState: row.ActiveState,
			SubState:    row.SubState,
			Path:        string(row.Path),
		}
		u.enrich(ctx, conn)
		out = append(out, u)
	}
	return out, nil
}

func (u *Unit) enrich(ctx context.Context, conn *dbus.Conn) {
	if u.Path == "" {
		return
	}
	obj := conn.Object(dest, dbus.ObjectPath(u.Path))
	u.ActiveEnterTimestamp = getUint64Prop(ctx, obj, "org.freedesktop.systemd1.Unit", "ActiveEnterTimestamp")
	switch {
	case strings.HasSuffix(u.Name, ".service"):
		u.MainPID = getUint32Prop(ctx, obj, "org.freedesktop.systemd1.Service", "MainPID")
		u.NRestarts = getUint32Prop(ctx, obj, "org.freedesktop.systemd1.Service", "NRestarts")
	case strings.HasSuffix(u.Name, ".socket"):
		u.NAccepted = getUint32Prop(ctx, obj, "org.freedesktop.systemd1.Socket", "NAccepted")
	}
}

func getUint32Prop(ctx context.Context, obj dbus.BusObject, iface, name string) uint32 {
	var v dbus.Variant
	if err := obj.CallWithContext(ctx, "org.freedesktop.DBus.Properties.Get", 0, iface, name).Store(&v); err != nil {
		return 0
	}
	return variantUint32(v)
}

func getUint64Prop(ctx context.Context, obj dbus.BusObject, iface, name string) uint64 {
	var v dbus.Variant
	if err := obj.CallWithContext(ctx, "org.freedesktop.DBus.Properties.Get", 0, iface, name).Store(&v); err != nil {
		return 0
	}
	return variantUint64(v)
}

func variantUint32(v dbus.Variant) uint32 {
	switch n := v.Value().(type) {
	case uint32:
		return n
	case uint64:
		return uint32(n)
	default:
		return 0
	}
}

func variantUint64(v dbus.Variant) uint64 {
	switch n := v.Value().(type) {
	case uint64:
		return n
	case uint32:
		return uint64(n)
	default:
		return 0
	}
}

type listRow struct {
	Name        string
	Description string
	LoadState   string
	ActiveState string
	SubState    string
	Following   string
	Path        dbus.ObjectPath
	JobID       uint32
	JobType     string
	JobPath     dbus.ObjectPath
}

func (u *User) runJob(ctx context.Context, method, unit string) error {
	conn, err := u.connection()
	if err != nil {
		return err
	}

	sigc := make(chan *dbus.Signal, 16)
	conn.Signal(sigc)
	defer conn.RemoveSignal(sigc)

	var job dbus.ObjectPath
	err = conn.Object(dest, objPath).CallWithContext(ctx, iface+"."+method, 0, unit, mode).Store(&job)
	if err != nil {
		return fmt.Errorf("%s %s: %w", method, unit, err)
	}

	for {
		select {
		case <-ctx.Done():
			return fmt.Errorf("%s %s: %w", method, unit, ctx.Err())
		case sig, ok := <-sigc:
			if !ok {
				return fmt.Errorf("%s %s: connection closed", method, unit)
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

func (u *User) connection() (*dbus.Conn, error) {
	u.mu.Lock()
	defer u.mu.Unlock()
	if u.conn != nil {
		return u.conn, nil
	}
	path := filepath.Join(u.runtimeDir, "systemd", "private")
	conn, err := dbus.Dial("unix:path=" + path)
	if err != nil {
		return nil, fmt.Errorf("connect to systemd user manager: %w", err)
	}
	if err := conn.Auth([]dbus.Auth{dbus.AuthExternal(strconv.Itoa(u.uid))}); err != nil {
		_ = conn.Close()
		return nil, fmt.Errorf("authenticate to systemd user manager: %w", err)
	}
	if err := conn.Object(dest, objPath).Call(iface+".Subscribe", 0).Err; err != nil {
		_ = conn.Close()
		return nil, fmt.Errorf("subscribe to systemd jobs: %w", err)
	}
	u.conn = conn
	return conn, nil
}
