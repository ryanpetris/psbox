package units

// Instance list and stop tests.

import (
	"bytes"
	"context"
	"errors"
	"io"
	"strings"
	"testing"

	"petris.dev/psbox/internal/instance"
	"petris.dev/psbox/internal/systemd"
)

type fakeUser struct {
	units []systemd.Unit
	stops []string
	err   error
}

func (f *fakeUser) Start(context.Context, string) error { return nil }

func (f *fakeUser) Stop(_ context.Context, unit string) error {
	f.stops = append(f.stops, unit)
	return f.err
}

func (f *fakeUser) List(context.Context, []string) ([]systemd.Unit, error) {
	return f.units, f.err
}

func TestListPairsAndSorts(t *testing.T) {
	t.Parallel()

	svc := NewService(&fakeUser{units: []systemd.Unit{
		{Name: "psboxd@" + instance.Escape("brave/work") + ".socket", SubState: "listening", NAccepted: 1},
		{Name: "other.service", ActiveState: "active"},
		{
			Name:                 "psboxd@" + instance.Escape("brave/default") + ".service",
			SubState:             "running",
			MainPID:              152692,
			ActiveEnterTimestamp: 1_787_000_000_000_000,
			NRestarts:            2,
		},
		{Name: "psboxd@" + instance.Escape("brave/default") + ".socket", SubState: "listening", NAccepted: 4},
	}})
	var buf bytes.Buffer
	if err := svc.List(t.Context(), false, &buf); err != nil {
		t.Fatal(err)
	}
	got := buf.String()
	for _, h := range []string{"IDENTITY", "SERVICE", "SOCKET", "PID", "SINCE", "RESTARTS", "ACCEPTED"} {
		if !strings.Contains(got, h) {
			t.Fatalf("missing header %s: %q", h, got)
		}
	}
	if strings.Contains(got, "brave/default") {
		t.Fatalf("default suffix shown: %q", got)
	}
	if !hasIdentityColumn(got, "brave") || !strings.Contains(got, "152692") || !strings.Contains(got, "running") {
		t.Fatalf("default row: %q", got)
	}
	if !strings.Contains(got, "4") {
		t.Fatalf("accepted: %q", got)
	}
	if !strings.Contains(got, "brave/work") {
		t.Fatalf("missing service: %q", got)
	}
	if strings.Index(got, "brave") > strings.Index(got, "brave/work") {
		t.Fatalf("unsorted: %q", got)
	}

	buf.Reset()
	if err := svc.List(t.Context(), true, &buf); err != nil {
		t.Fatal(err)
	}
	if buf.String() != "brave\nbrave/work\n" {
		t.Fatalf("quiet: %q", buf.String())
	}

	names, err := svc.Identities(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if strings.Join(names, "\n") != "brave\nbrave/work" {
		t.Fatalf("identities: %q", names)
	}
}

func hasIdentityColumn(table, name string) bool {
	for _, line := range strings.Split(table, "\n") {
		fields := strings.Fields(line)
		if len(fields) > 0 && fields[0] == name {
			return true
		}
	}
	return false
}

func TestDisplayIdentity(t *testing.T) {
	t.Parallel()

	if got := displayIdentity("brave/default"); got != "brave" {
		t.Fatalf("default: %q", got)
	}
	if got := displayIdentity("brave/work"); got != "brave/work" {
		t.Fatalf("named: %q", got)
	}
	if got := displayIdentity("not-an-identity"); got != "not-an-identity" {
		t.Fatalf("invalid: %q", got)
	}
}

func TestResolveInstance(t *testing.T) {
	t.Parallel()

	items := []Instance{
		{Identity: "firefox/default", Escaped: instance.Escape("firefox/default")},
		{Identity: "firefox/work", Escaped: instance.Escape("firefox/work")},
	}
	got, err := resolveInstance("firefox", items)
	if err != nil || got.Identity != "firefox/default" {
		t.Fatalf("bare name: %+v %v", got, err)
	}
	got, err = resolveInstance("firefox/default", items)
	if err != nil || got.Identity != "firefox/default" {
		t.Fatalf("explicit default: %+v %v", got, err)
	}
	got, err = resolveInstance("firefox/work", items)
	if err != nil || got.Identity != "firefox/work" {
		t.Fatalf("full name: %+v %v", got, err)
	}
	got, err = resolveInstance(instance.Escape("firefox/work"), items)
	if err != nil || got.Identity != "firefox/work" {
		t.Fatalf("escaped: %+v %v", got, err)
	}
	if _, err := resolveInstance("missing", items); err == nil {
		t.Fatal("expected error")
	}
}

func TestStopOrderAndAll(t *testing.T) {
	t.Parallel()

	def := instance.Escape("app/default")
	work := instance.Escape("app/work")
	fake := &fakeUser{units: []systemd.Unit{
		{Name: "psboxd@" + def + ".service"},
		{Name: "psboxd@" + def + ".socket"},
		{Name: "psboxd@" + work + ".service"},
		{Name: "psboxd@" + work + ".socket"},
	}}
	svc := NewService(fake)
	var stderr bytes.Buffer
	if err := svc.Stop(t.Context(), "app", &stderr); err != nil {
		t.Fatal(err)
	}
	sock, service := instance.UnitNames(def)
	if len(fake.stops) != 2 || fake.stops[0] != service || fake.stops[1] != sock {
		t.Fatalf("stop order: %v", fake.stops)
	}
	if !strings.Contains(stderr.String(), "Stopping app...") {
		t.Fatalf("stderr: %q", stderr.String())
	}
	if strings.Contains(stderr.String(), "app/default") {
		t.Fatalf("default suffix shown: %q", stderr.String())
	}

	fake.stops = nil
	if err := svc.StopAll(t.Context(), ioDiscard{}); err != nil {
		t.Fatal(err)
	}
	if len(fake.stops) != 4 {
		t.Fatalf("stop all: %v", fake.stops)
	}

	empty := NewService(&fakeUser{})
	if err := empty.StopAll(t.Context(), ioDiscard{}); err != nil {
		t.Fatal(err)
	}
}

func TestFormatUnitFields(t *testing.T) {
	t.Parallel()

	if formatPID(systemd.Unit{}) != "-" || formatPID(systemd.Unit{Name: "u", MainPID: 0}) != "-" {
		t.Fatal("empty pid")
	}
	if formatPID(systemd.Unit{Name: "u", MainPID: 9}) != "9" {
		t.Fatal("pid")
	}
	if formatSince(systemd.Unit{}) != "-" || formatSince(systemd.Unit{Name: "u"}) != "-" {
		t.Fatal("empty since")
	}
	got := formatSince(systemd.Unit{Name: "u", ActiveEnterTimestamp: 1_700_000_000_000_000})
	if got == "-" || len(got) < 10 {
		t.Fatalf("since %q", got)
	}
	if formatCount(systemd.Unit{}, 3) != "-" || formatCount(systemd.Unit{Name: "u"}, 0) != "0" {
		t.Fatal("count")
	}
}

type ioDiscard struct{}

func (ioDiscard) Write(p []byte) (int, error) { return len(p), nil }

type failedOutput struct{}

var _ io.Writer = failedOutput{}

func (failedOutput) Write([]byte) (int, error) { return 0, io.ErrClosedPipe }

func TestListReportsOutputFailure(t *testing.T) {
	svc := NewService(&fakeUser{units: []systemd.Unit{{Name: "psboxd@" + instance.Escape("app/default") + ".socket"}}})
	for _, quiet := range []bool{false, true} {
		if err := svc.List(t.Context(), quiet, failedOutput{}); !errors.Is(err, io.ErrClosedPipe) {
			t.Fatalf("quiet=%v: %v", quiet, err)
		}
	}
}
