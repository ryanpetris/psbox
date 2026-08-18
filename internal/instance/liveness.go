package instance

// /proc liveness classification for idle teardown.

import (
	"bytes"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

// Proc is one /proc scan entry.
type Proc struct {
	PID       int
	PPID      int
	StartTime uint64
}

// InfraRoot is a process the agent started, identified by pid and starttime.
type InfraRoot struct {
	PID       int
	StartTime uint64
}

// WorkloadPresent reports whether any process is workload, not infrastructure.
func WorkloadPresent(self, pid1 int, roots []InfraRoot, procs []Proc) bool {
	infra := map[string]struct{}{}
	for _, r := range roots {
		if r.PID == self || r.PID == pid1 || r.PID <= 0 {
			continue
		}
		infra[procKey(r.PID, r.StartTime)] = struct{}{}
	}
	byPID := map[int]Proc{}
	for _, p := range procs {
		byPID[p.PID] = p
	}
	changed := true
	for changed {
		changed = false
		for _, p := range procs {
			if _, ok := infra[procKey(p.PID, p.StartTime)]; ok {
				continue
			}
			parent, ok := byPID[p.PPID]
			if !ok {
				continue
			}
			if _, ok := infra[procKey(parent.PID, parent.StartTime)]; ok {
				infra[procKey(p.PID, p.StartTime)] = struct{}{}
				changed = true
			}
		}
	}
	for _, p := range procs {
		if p.PID == self || p.PID == pid1 {
			continue
		}
		if _, ok := infra[procKey(p.PID, p.StartTime)]; ok {
			continue
		}
		return true
	}
	return false
}

func procKey(pid int, start uint64) string {
	return strconv.Itoa(pid) + ":" + strconv.FormatUint(start, 10)
}

// ScanProc reads numeric /proc entries from procDir (usually "/proc").
func ScanProc(procDir string) ([]Proc, error) {
	entries, err := os.ReadDir(procDir)
	if err != nil {
		return nil, err
	}
	var out []Proc
	for _, ent := range entries {
		if !ent.IsDir() {
			continue
		}
		pid, err := strconv.Atoi(ent.Name())
		if err != nil {
			continue
		}
		p, err := readProcStat(filepath.Join(procDir, ent.Name(), "stat"))
		if err != nil {
			continue
		}
		p.PID = pid
		out = append(out, p)
	}
	return out, nil
}

func readProcStat(path string) (Proc, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return Proc{}, err
	}
	// comm is in parentheses and may contain spaces.
	start := bytes.LastIndexByte(data, ')')
	if start < 0 || start+2 >= len(data) {
		return Proc{}, fmt.Errorf("parse %s", path)
	}
	fields := strings.Fields(string(data[start+2:]))
	// fields[0] = state, [1] = ppid, [19] = starttime (0-based after comm)
	if len(fields) < 20 {
		return Proc{}, fmt.Errorf("parse %s", path)
	}
	ppid, err := strconv.Atoi(fields[1])
	if err != nil {
		return Proc{}, err
	}
	startTime, err := strconv.ParseUint(fields[19], 10, 64)
	if err != nil {
		return Proc{}, err
	}
	return Proc{PPID: ppid, StartTime: startTime}, nil
}

func lookupStartTime(procDir string, pid int) (uint64, error) {
	p, err := readProcStat(filepath.Join(procDir, strconv.Itoa(pid), "stat"))
	if err != nil {
		return 0, err
	}
	return p.StartTime, nil
}
