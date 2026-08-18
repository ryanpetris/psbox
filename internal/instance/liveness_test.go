package instance

// Workload liveness tests.

import (
	"os"
	"path/filepath"
	"testing"
)

func TestWorkloadClassification(t *testing.T) {
	t.Parallel()

	const self, pid1 = 2, 1
	dbus := InfraRoot{PID: 10, StartTime: 100}
	procs := []Proc{
		{PID: pid1, PPID: 0, StartTime: 1},
		{PID: self, PPID: pid1, StartTime: 2},
		{PID: 10, PPID: self, StartTime: 100},
		{PID: 11, PPID: 10, StartTime: 110},
	}
	if hasWorkload(self, pid1, expandInfra(self, pid1, []InfraRoot{dbus}, procs, nil), procs) {
		t.Fatal("dbus descendants should be infrastructure")
	}

	procs = append(procs, Proc{PID: 20, PPID: pid1, StartTime: 200})
	if !hasWorkload(self, pid1, expandInfra(self, pid1, []InfraRoot{dbus}, procs, nil), procs) {
		t.Fatal("pid 20 should be workload")
	}

	if !hasWorkload(self, pid1, expandInfra(self, pid1, []InfraRoot{{PID: self, StartTime: 2}}, procs[:4], nil), procs[:4]) {
		t.Fatal("agent must not be treated as an infrastructure root")
	}
	lone := []Proc{
		{PID: pid1, PPID: 0, StartTime: 1},
		{PID: self, PPID: pid1, StartTime: 2},
		{PID: 30, PPID: pid1, StartTime: 300},
	}
	if !hasWorkload(self, pid1, expandInfra(self, pid1, []InfraRoot{{PID: pid1, StartTime: 1}}, lone, nil), lone) {
		t.Fatal("PID 1 must not be treated as an infrastructure root")
	}
}

func TestExpandInfraRemembersReparentedChild(t *testing.T) {
	t.Parallel()

	const self, pid1 = 2, 1
	dbus := InfraRoot{PID: 10, StartTime: 100}
	asChild := []Proc{
		{PID: pid1, PPID: 0, StartTime: 1},
		{PID: self, PPID: pid1, StartTime: 2},
		{PID: 10, PPID: self, StartTime: 100},
		{PID: 11, PPID: 10, StartTime: 110},
	}
	infra := expandInfra(self, pid1, []InfraRoot{dbus}, asChild, nil)
	if hasWorkload(self, pid1, infra, asChild) {
		t.Fatal("child of dbus-daemon should be infrastructure")
	}

	reparented := []Proc{
		{PID: pid1, PPID: 0, StartTime: 1},
		{PID: self, PPID: pid1, StartTime: 2},
		{PID: 10, PPID: self, StartTime: 100},
		{PID: 11, PPID: pid1, StartTime: 110},
	}
	if !hasWorkload(self, pid1, expandInfra(self, pid1, []InfraRoot{dbus}, reparented, nil), reparented) {
		t.Fatal("reparented helper is workload without memory")
	}
	seeds := append([]InfraRoot{dbus}, rememberInfra(infra, asChild)...)
	kept := expandInfra(self, pid1, seeds, reparented, nil)
	if hasWorkload(self, pid1, kept, reparented) {
		t.Fatal("remembered dbus child should stay infrastructure after reparent")
	}
}

func TestExpandInfraDBusStarter(t *testing.T) {
	t.Parallel()

	const self, pid1 = 2, 1
	procs := []Proc{
		{PID: pid1, PPID: 0, StartTime: 1},
		{PID: self, PPID: pid1, StartTime: 2},
		{PID: 10, PPID: self, StartTime: 100},
		{PID: 40, PPID: pid1, StartTime: 400},
		{PID: 41, PPID: 40, StartTime: 410},
	}
	dbus := []InfraRoot{{PID: 10, StartTime: 100}}
	if !hasWorkload(self, pid1, expandInfra(self, pid1, dbus, procs, nil), procs) {
		t.Fatal("unrelated pid-1 child should be workload")
	}
	infra := expandInfra(self, pid1, dbus, procs, func(p Proc) bool {
		return p.PID == 40
	})
	if hasWorkload(self, pid1, infra, procs) {
		t.Fatal("DBUS_STARTER process and its children should be infrastructure")
	}
}

func TestEnvironHasDBusStarter(t *testing.T) {
	t.Parallel()

	if environHasDBusStarter([]byte("PATH=/usr/bin\x00HOME=/tmp\x00")) {
		t.Fatal("session env is not activation")
	}
	if !environHasDBusStarter([]byte("DBUS_SESSION_BUS_ADDRESS=unix:path=/tmp/bus\x00DBUS_STARTER_ADDRESS=unix:path=/tmp/bus\x00")) {
		t.Fatal("DBUS_STARTER_ADDRESS")
	}
	if !environHasDBusStarter([]byte("DBUS_STARTER_BUS_TYPE=session\x00")) {
		t.Fatal("DBUS_STARTER_BUS_TYPE")
	}
}

func TestHasDBusStarterReadsProc(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	proc := filepath.Join(dir, "9")
	if err := os.Mkdir(proc, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(proc, "environ"), []byte("DBUS_STARTER_BUS_TYPE=session\x00"), 0o644); err != nil {
		t.Fatal(err)
	}
	if !hasDBusStarter(dir, 9) {
		t.Fatal("should read starter from environ")
	}
	if hasDBusStarter(dir, 8) {
		t.Fatal("missing pid")
	}
}
