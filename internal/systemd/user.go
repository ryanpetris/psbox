package systemd

// User-manager D-Bus client (session bus first, private socket fallback).

import (
	"context"
	"fmt"
	"os"
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

	mu    sync.Mutex
	conn  *dbus.Conn
	onBus bool
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
	return u.retry(ctx, func(ctx context.Context) error {
		err := u.runJob(ctx, "StartUnit", unit)
		if err == nil {
			return nil
		}
		if active, aerr := u.unitIsActive(ctx, unit); aerr == nil && active {
			return nil
		}
		return err
	})
}

// Stop stops unit and waits for the systemd job to finish.
func (u *User) Stop(ctx context.Context, unit string) error {
	return u.retry(ctx, func(ctx context.Context) error {
		err := u.runJob(ctx, "StopUnit", unit)
		if err == nil {
			return nil
		}
		if active, aerr := u.unitIsActive(ctx, unit); aerr != nil || !active {
			return nil
		}
		return err
	})
}

// List returns units matching patterns (for example psboxd@*.socket).
func (u *User) List(ctx context.Context, patterns []string) ([]Unit, error) {
	var out []Unit
	err := u.retry(ctx, func(ctx context.Context) error {
		items, err := u.listOnce(ctx, patterns)
		if err != nil {
			return err
		}
		out = items
		return nil
	})
	return out, err
}

func (u *User) listOnce(ctx context.Context, patterns []string) ([]Unit, error) {
	var rows []listRow
	err := u.managerCall(ctx, CallTimeout, "ListUnitsByPatterns", []any{[]string{}, patterns}, &rows)
	if err != nil {
		return nil, fmt.Errorf("list units: %w", err)
	}
	out := make([]Unit, 0, len(rows))
	for _, row := range rows {
		item := Unit{
			Name:        row.Name,
			ActiveState: row.ActiveState,
			SubState:    row.SubState,
			Path:        string(row.Path),
		}
		item.enrich(ctx, u)
		out = append(out, item)
	}
	return out, nil
}

func (unit *Unit) enrich(ctx context.Context, u *User) {
	if unit.Path == "" {
		return
	}
	unit.ActiveEnterTimestamp = u.uint64Prop(ctx, dbus.ObjectPath(unit.Path), "org.freedesktop.systemd1.Unit", "ActiveEnterTimestamp")
	switch {
	case strings.HasSuffix(unit.Name, ".service"):
		unit.MainPID = u.uint32Prop(ctx, dbus.ObjectPath(unit.Path), "org.freedesktop.systemd1.Service", "MainPID")
		unit.NRestarts = u.uint32Prop(ctx, dbus.ObjectPath(unit.Path), "org.freedesktop.systemd1.Service", "NRestarts")
	case strings.HasSuffix(unit.Name, ".socket"):
		unit.NAccepted = u.uint32Prop(ctx, dbus.ObjectPath(unit.Path), "org.freedesktop.systemd1.Socket", "NAccepted")
	}
}

func (u *User) uint32Prop(ctx context.Context, path dbus.ObjectPath, iface, name string) uint32 {
	v, err := u.getProp(ctx, path, iface, name)
	if err != nil {
		return 0
	}
	return variantUint32(v)
}

func (u *User) uint64Prop(ctx context.Context, path dbus.ObjectPath, iface, name string) uint64 {
	v, err := u.getProp(ctx, path, iface, name)
	if err != nil {
		return 0
	}
	return variantUint64(v)
}

func (u *User) getProp(ctx context.Context, path dbus.ObjectPath, iface, name string) (dbus.Variant, error) {
	ctx, cancel := context.WithTimeout(ctx, CallTimeout)
	defer cancel()
	conn, err := u.connection(ctx)
	if err != nil {
		return dbus.Variant{}, err
	}
	var v dbus.Variant
	err = conn.Object(dest, path).CallWithContext(ctx, "org.freedesktop.DBus.Properties.Get", 0, iface, name).Store(&v)
	return v, err
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

func (u *User) unitIsActive(ctx context.Context, unit string) (bool, error) {
	var path dbus.ObjectPath
	if err := u.managerCall(ctx, CallTimeout, "GetUnit", []any{unit}, &path); err != nil {
		return false, err
	}
	if path == "" {
		return false, nil
	}
	v, err := u.getProp(ctx, path, "org.freedesktop.systemd1.Unit", "ActiveState")
	if err != nil {
		return false, err
	}
	state, _ := v.Value().(string)
	return state == "active", nil
}
