package instance

// Private session bus started by the agent.

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"syscall"
	"time"
)

func (a *Agent) startPrivateBus(ctx context.Context) error {
	r, w, err := os.Pipe()
	if err != nil {
		return err
	}
	cmd := exec.CommandContext(ctx, "dbus-daemon", "--session", "--nofork", "--print-address=3")
	cmd.Stdout = os.Stderr
	cmd.Stderr = os.Stderr
	cmd.ExtraFiles = []*os.File{w}
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := cmd.Start(); err != nil {
		_ = r.Close()
		_ = w.Close()
		return fmt.Errorf("start dbus-daemon: %w", err)
	}
	_ = w.Close()

	addr, err := readBusAddress(r, 3*time.Second)
	_ = r.Close()
	if err != nil {
		_ = cmd.Process.Kill()
		_ = cmd.Wait()
		return err
	}

	start, err := lookupStartTime(a.procDir, cmd.Process.Pid)
	if err != nil {
		start = 0
	}
	a.mu.Lock()
	a.busAddr = addr
	a.busProc = cmd.Process
	a.infra = append(a.infra, InfraRoot{PID: cmd.Process.Pid, StartTime: start})
	a.mu.Unlock()

	a.busDone = make(chan error, 1)
	a.workers.Go(func() { a.busDone <- cmd.Wait() })

	return nil
}

func readBusAddress(r *os.File, timeout time.Duration) (string, error) {
	if err := r.SetReadDeadline(time.Now().Add(timeout)); err != nil {
		return "", err
	}
	buf := make([]byte, 4096)
	n, err := r.Read(buf)
	if err != nil {
		return "", fmt.Errorf("read dbus address: %w", err)
	}
	addr := strings.TrimSpace(string(buf[:n]))
	if addr == "" {
		return "", fmt.Errorf("empty dbus address")
	}
	return addr, nil
}
