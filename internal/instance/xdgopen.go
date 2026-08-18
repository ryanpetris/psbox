package instance

// xdg-open entrypoint for argv0 xdg-open and `psboxa xdg-open`.

import (
	"bufio"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"time"
)

const hostXDGOpen = "/usr/bin/xdg-open"

// InvokedAsXDGOpen reports whether this process should act as xdg-open.
func InvokedAsXDGOpen(args []string) bool {
	if len(args) == 0 {
		return false
	}
	if filepath.Base(args[0]) == "xdg-open" {
		return true
	}
	return len(args) >= 2 && args[1] == "xdg-open"
}

// RunXDGOpen forwards http(s) to the agent or execs the host xdg-open.
func RunXDGOpen(args []string) int {
	rest := xdgOpenArgs(args)
	target := xdgOpenTarget(rest)
	if target == "" || !ForwardableURI(target) {
		return execHostXDGOpen(rest)
	}
	if err := requestHostOpen(target); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	return 0
}

func xdgOpenArgs(args []string) []string {
	if len(args) == 0 {
		return nil
	}
	if filepath.Base(args[0]) == "xdg-open" {
		return args[1:]
	}
	if len(args) >= 2 && args[1] == "xdg-open" {
		return args[2:]
	}
	return args[1:]
}

func xdgOpenTarget(args []string) string {
	for _, arg := range args {
		if arg == "--" || strings.HasPrefix(arg, "-") {
			continue
		}
		return arg
	}
	return ""
}

func requestHostOpen(uri string) error {
	conn, err := net.DialTimeout("unix", xdgOpenSocketPath(), FirstReplyTimeout)
	if err != nil {
		return fmt.Errorf("open uri: %w", err)
	}
	defer conn.Close()
	_ = conn.SetDeadline(time.Now().Add(FirstReplyTimeout))
	if _, err := fmt.Fprintln(conn, uri); err != nil {
		return err
	}
	line, err := bufio.NewReader(conn).ReadString('\n')
	if err != nil {
		return err
	}
	resp := strings.TrimSpace(line)
	switch {
	case resp == "ok":
		return nil
	case resp == "local":
		return fmt.Errorf("open uri: refused")
	case strings.HasPrefix(resp, "error "):
		return fmt.Errorf("%s", strings.TrimPrefix(resp, "error "))
	default:
		return fmt.Errorf("open uri: unexpected reply")
	}
}

func execHostXDGOpen(args []string) int {
	if _, err := os.Stat(hostXDGOpen); err != nil {
		fmt.Fprintf(os.Stderr, "xdg-open: %v\n", err)
		return 1
	}
	argv := append([]string{hostXDGOpen}, args...)
	err := syscall.Exec(hostXDGOpen, argv, os.Environ())
	if err != nil {
		fmt.Fprintf(os.Stderr, "xdg-open: %v\n", err)
		return 1
	}
	return 0
}
