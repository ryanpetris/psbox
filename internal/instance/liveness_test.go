package instance

// Workload liveness tests.

import "testing"

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
	if WorkloadPresent(self, pid1, []InfraRoot{dbus}, procs) {
		t.Fatal("dbus descendants should be infrastructure")
	}

	procs = append(procs, Proc{PID: 20, PPID: pid1, StartTime: 200})
	if !WorkloadPresent(self, pid1, []InfraRoot{dbus}, procs) {
		t.Fatal("pid 20 should be workload")
	}

	if !WorkloadPresent(self, pid1, []InfraRoot{{PID: self, StartTime: 2}}, procs[:4]) {
		t.Fatal("agent must not be treated as an infrastructure root")
	}
	if !WorkloadPresent(self, pid1, []InfraRoot{{PID: pid1, StartTime: 1}}, []Proc{
		{PID: pid1, PPID: 0, StartTime: 1},
		{PID: self, PPID: pid1, StartTime: 2},
		{PID: 30, PPID: pid1, StartTime: 300},
	}) {
		t.Fatal("PID 1 must not be treated as an infrastructure root")
	}
}
