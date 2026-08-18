package instance

// Private session bus started by the agent.

import (
	"context"
	"fmt"
	"io"
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

	go func() {
		err := cmd.Wait()
		a.log.Error("private dbus-daemon exited", "error", err)
		os.Exit(1)
	}()
	return nil
}

func readBusAddress(r io.Reader, timeout time.Duration) (string, error) {
	done := make(chan struct{})
	var addr string
	var err error
	go func() {
		buf := make([]byte, 4096)
		n, e := r.Read(buf)
		if e != nil && n == 0 {
			err = e
		} else {
			addr = strings.TrimSpace(string(buf[:n]))
		}
		close(done)
	}()
	select {
	case <-done:
		if err != nil {
			return "", fmt.Errorf("read dbus address: %w", err)
		}
		if addr == "" {
			return "", fmt.Errorf("empty dbus address")
		}
		return addr, nil
	case <-time.After(timeout):
		return "", fmt.Errorf("timeout reading dbus address")
	}
}
