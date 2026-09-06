package systemd

// D-Bus transport setup respects deadlines and cancellation without owning lifetime.

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/godbus/dbus/v5"
)

func TestParseBusAddress(t *testing.T) {
	for _, tc := range []struct{ input, network, address, nonce string }{
		{"unix:path=/run/user/1000/bus", "unix", "/run/user/1000/bus", ""},
		{"unix:guid=abc,path=/tmp/space%20bus", "unix", "/tmp/space bus", ""},
		{"unix:abstract=/tmp/bus", "unix", "\x00/tmp/bus", ""},
		{"tcp:host=127.0.0.1,port=1234,family=ipv4", "tcp4", "127.0.0.1:1234", ""},
		{"nonce-tcp:host=::1,port=1234,family=ipv6,noncefile=/tmp/nonce", "tcp6", "[::1]:1234", "/tmp/nonce"},
	} {
		got, err := parseBusAddress(tc.input)
		if err != nil || got.network != tc.network || got.address != tc.address || got.noncefile != tc.nonce {
			t.Fatalf("%s: %+v, %v", tc.input, got, err)
		}
	}
	for _, addr := range []string{"", "unix:path=x,abstract=y", "unix:path=%zz", "unix:path=x,path=y", "tcp:host=x", "tcp:host=x,port=1,family=bad", "nonce-tcp:host=x,port=1"} {
		if _, err := parseBusAddress(addr); err == nil {
			t.Errorf("accepted %q", addr)
		}
	}
}

func TestBusAuthenticationCancellation(t *testing.T) {
	for _, transport := range []string{"path", "abstract", "tcp", "nonce-tcp"} {
		for _, canceled := range []bool{false, true} {
			t.Run(fmt.Sprintf("%s/cancel=%v", transport, canceled), func(t *testing.T) {
				network, address := "unix", filepath.Join(t.TempDir(), "bus")
				if transport == "abstract" {
					address = "\x00psbox-test-" + filepath.Base(t.TempDir()) + fmt.Sprint(time.Now().UnixNano())
				}
				if transport == "tcp" || transport == "nonce-tcp" {
					network, address = "tcp", "127.0.0.1:0"
				}
				ln, err := net.Listen(network, address)
				if err != nil {
					t.Fatal(err)
				}
				defer ln.Close()
				addr := "unix:path=" + dbus.EscapeBusAddressValue(address)
				if transport == "abstract" {
					addr = "unix:abstract=" + dbus.EscapeBusAddressValue(address[1:])
				}
				if network == "tcp" {
					host, port, _ := net.SplitHostPort(ln.Addr().String())
					addr = transport + ":host=" + host + ",port=" + port
					if transport == "nonce-tcp" {
						path := filepath.Join(t.TempDir(), "nonce")
						if err := os.WriteFile(path, make([]byte, 16), 0o600); err != nil {
							t.Fatal(err)
						}
						addr += ",noncefile=" + dbus.EscapeBusAddressValue(path)
					}
				}
				ctx, cancel := context.WithTimeout(t.Context(), 150*time.Millisecond)
				defer cancel()
				peerDone := make(chan error, 1)
				go func() {
					conn, err := ln.Accept()
					if err != nil {
						peerDone <- err
						return
					}
					defer conn.Close()
					if canceled {
						cancel()
					}
					_, err = io.Copy(io.Discard, conn)
					peerDone <- err
				}()
				start := time.Now()
				u := NewUser()
				conn, err := u.dialAndAuth(ctx, addr)
				if conn != nil {
					_ = conn.Close()
					t.Fatal("stalled authentication succeeded")
				}
				if err == nil {
					t.Fatal("missing authentication error")
				}
				if canceled && !errors.Is(err, context.Canceled) {
					t.Fatalf("cancellation: %v", err)
				}
				if time.Since(start) > time.Second {
					t.Fatalf("authentication exceeded bound: %v", err)
				}
				select {
				case <-peerDone:
				case <-time.After(time.Second):
					t.Fatal("transport was not closed")
				}
			})
		}
	}
}

func TestAuthenticatedConnectionOutlivesSetupContext(t *testing.T) {
	ln, err := net.Listen("unix", filepath.Join(t.TempDir(), "bus"))
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()
	peerDone := make(chan error, 1)
	go func() {
		conn, err := ln.Accept()
		if err != nil {
			peerDone <- err
			return
		}
		defer conn.Close()
		rd := bufio.NewReader(conn)
		if _, err := rd.ReadString('\n'); err != nil {
			peerDone <- err
			return
		}
		if _, err := io.WriteString(conn, "REJECTED EXTERNAL\r\n"); err != nil {
			peerDone <- err
			return
		}
		if _, err := rd.ReadString('\n'); err != nil {
			peerDone <- err
			return
		}
		if _, err := io.WriteString(conn, "OK 0123456789abcdef0123456789abcdef\r\n"); err != nil {
			peerDone <- err
			return
		}
		if _, err := rd.ReadString('\n'); err != nil {
			peerDone <- err
			return
		}
		_, err = io.Copy(io.Discard, rd)
		peerDone <- err
	}()
	ctx, cancel := context.WithCancel(t.Context())
	conn, err := NewUser().dialAndAuth(ctx, "unix:path="+dbus.EscapeBusAddressValue(ln.Addr().String()))
	if err != nil {
		cancel()
		t.Fatal(err)
	}
	cancel()
	select {
	case <-conn.Context().Done():
		t.Fatal("setup context closed established connection")
	case <-time.After(20 * time.Millisecond):
	}
	if err := conn.Close(); err != nil {
		t.Fatal(err)
	}
	<-peerDone
}
