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

func expandInfra(self, pid1 int, seeds []InfraRoot, procs []Proc, extra func(Proc) bool) map[string]struct{} {
	infra := map[string]struct{}{}
	for _, r := range seeds {
		if r.PID == self || r.PID == pid1 || r.PID <= 0 {
			continue
		}
		infra[procKey(r.PID, r.StartTime)] = struct{}{}
	}
	if extra != nil {
		for _, p := range procs {
			if p.PID == self || p.PID == pid1 {
				continue
			}
			if extra(p) {
				infra[procKey(p.PID, p.StartTime)] = struct{}{}
			}
		}
	}

	byPID := map[int]Proc{}
	for _, p := range procs {
		byPID[p.PID] = p
	}
	changed := true
	for changed {
		changed = false
		for _, p := range procs {
			key := procKey(p.PID, p.StartTime)
			if _, ok := infra[key]; ok {
				continue
			}
			parent, ok := byPID[p.PPID]
			if !ok {
				continue
			}
			if _, ok := infra[procKey(parent.PID, parent.StartTime)]; ok {
				infra[key] = struct{}{}
				changed = true
			}
		}
	}
	return infra
}

func hasWorkload(self, pid1 int, infra map[string]struct{}, procs []Proc) bool {
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

func rememberInfra(infra map[string]struct{}, procs []Proc) []InfraRoot {
	out := make([]InfraRoot, 0, len(infra))
	for _, p := range procs {
		if _, ok := infra[procKey(p.PID, p.StartTime)]; ok {
			out = append(out, InfraRoot{PID: p.PID, StartTime: p.StartTime})
		}
	}
	return out
}

func hasDBusStarter(procDir string, pid int) bool {
	data, err := os.ReadFile(filepath.Join(procDir, strconv.Itoa(pid), "environ"))
	if err != nil {
		return false
	}
	return environHasDBusStarter(data)
}

func environHasDBusStarter(data []byte) bool {
	for len(data) > 0 {
		var kv []byte
		kv, data, _ = bytes.Cut(data, []byte{0})
		if bytes.HasPrefix(kv, []byte("DBUS_STARTER_ADDRESS=")) ||
			bytes.HasPrefix(kv, []byte("DBUS_STARTER_BUS_TYPE=")) {
			return true
		}
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
