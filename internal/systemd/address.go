package systemd

// Parse supported D-Bus transports for context-aware network dialing.

import (
	"fmt"
	"io"
	"net"
	"os"
	"strings"
	"syscall"

	"github.com/godbus/dbus/v5"
)

type busAddress struct {
	network   string
	address   string
	noncefile string
}

func parseBusAddress(addr string) (busAddress, error) {
	transport, rest, ok := strings.Cut(addr, ":")
	if !ok {
		return busAddress{}, fmt.Errorf("invalid bus address %q", addr)
	}
	values := map[string]string{}
	for _, part := range strings.Split(rest, ",") {
		key, value, ok := strings.Cut(part, "=")
		if !ok || key == "" {
			return busAddress{}, fmt.Errorf("invalid bus address parameter %q", part)
		}
		decoded, err := dbus.UnescapeBusAddressValue(value)
		if err != nil {
			return busAddress{}, err
		}
		if _, exists := values[key]; exists {
			return busAddress{}, fmt.Errorf("duplicate bus address key %q", key)
		}
		values[key] = decoded
	}
	switch transport {
	case "unix":
		path, abstract := values["path"], values["abstract"]
		if (path == "") == (abstract == "") {
			return busAddress{}, fmt.Errorf("unix bus address requires exactly one path or abstract name")
		}
		if abstract != "" {
			path = "\x00" + abstract
		}
		return busAddress{network: "unix", address: path}, nil
	case "tcp", "nonce-tcp":
		network := "tcp"
		switch values["family"] {
		case "":
		case "ipv4":
			network = "tcp4"
		case "ipv6":
			network = "tcp6"
		default:
			return busAddress{}, fmt.Errorf("invalid TCP family %q", values["family"])
		}
		if values["host"] == "" || values["port"] == "" {
			return busAddress{}, fmt.Errorf("TCP bus address requires host and port")
		}
		endpoint := busAddress{network: network, address: net.JoinHostPort(values["host"], values["port"])}
		if transport == "nonce-tcp" {
			endpoint.noncefile = values["noncefile"]
			if endpoint.noncefile == "" {
				return busAddress{}, fmt.Errorf("nonce-tcp bus address requires noncefile")
			}
		}
		return endpoint, nil
	default:
		return busAddress{}, fmt.Errorf("unsupported bus transport %q", transport)
	}
}

func readBusNonce(path string) ([]byte, error) {
	// Nonblocking open also permits rejecting a FIFO without waiting for a writer.
	f, err := os.OpenFile(path, os.O_RDONLY|syscall.O_NONBLOCK, 0)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	st, err := f.Stat()
	if err != nil {
		return nil, err
	}
	if !st.Mode().IsRegular() {
		return nil, fmt.Errorf("bus nonce must be a regular file")
	}
	data, err := io.ReadAll(io.LimitReader(f, 17))
	if err != nil {
		return nil, err
	}
	if len(data) != 16 {
		return nil, fmt.Errorf("bus nonce must contain 16 bytes")
	}
	return data, nil
}
