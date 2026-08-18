package instance

// systemd unit-name escaping and socket path checks.

import (
	"fmt"
	"path/filepath"
	"strings"
	"unicode"
)

// SunPathMax is the Linux sockaddr_un.sun_path size, including the NUL.
const SunPathMax = 108

// Escape converts a systemd unit instance name the same way systemd-escape does.
func Escape(s string) string {
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		c := s[i]
		if i == 0 && c == '.' {
			b.WriteString(`\x2e`)
			continue
		}
		if c == '/' {
			b.WriteByte('-')
			continue
		}
		if unitUnescaped(c) {
			b.WriteByte(c)
			continue
		}
		fmt.Fprintf(&b, `\x%02x`, c)
	}
	return b.String()
}

// Unescape reverses Escape.
func Unescape(s string) (string, error) {
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		switch s[i] {
		case '-':
			b.WriteByte('/')
		case '\\':
			if i+3 >= len(s) || s[i+1] != 'x' {
				return "", fmt.Errorf("invalid systemd escape in %q", s)
			}
			hi, ok1 := fromHex(s[i+2])
			lo, ok2 := fromHex(s[i+3])
			if !ok1 || !ok2 {
				return "", fmt.Errorf("invalid systemd escape in %q", s)
			}
			b.WriteByte(hi<<4 | lo)
			i += 3
		default:
			b.WriteByte(s[i])
		}
	}
	return b.String(), nil
}

// SplitIdentity splits an unescaped "<sandbox>/<instance>" name.
func SplitIdentity(raw string) (sandbox, instance string, err error) {
	sandbox, instance, ok := strings.Cut(raw, "/")
	if !ok || sandbox == "" || instance == "" {
		return "", "", fmt.Errorf("invalid instance identity %q", raw)
	}
	if strings.Contains(instance, "/") {
		return "", "", fmt.Errorf("invalid instance identity %q", raw)
	}
	return sandbox, instance, nil
}

// Identity is "<sandbox>/<instance>".
func Identity(sandbox, instance string) string {
	return sandbox + "/" + instance
}

// SocketPath is $XDG_RUNTIME_DIR/psbox/<escaped>.sock.
func SocketPath(runtimeDir, escaped string) string {
	return filepath.Join(runtimeDir, "psbox", escaped+".sock")
}

// CheckSocketPath reports whether path fits in sockaddr_un.sun_path.
func CheckSocketPath(path string) error {
	if len(path)+1 > SunPathMax {
		return fmt.Errorf("socket path %q exceeds sun_path (%d)", path, SunPathMax)
	}
	return nil
}

// UnitNames returns the socket and service unit names for an escaped identity.
func UnitNames(escaped string) (socket, service string) {
	return "psboxd@" + escaped + ".socket", "psboxd@" + escaped + ".service"
}

func unitUnescaped(c byte) bool {
	return unicode.IsLetter(rune(c)) || unicode.IsDigit(rune(c)) || c == ':' || c == '_' || c == '.'
}

func fromHex(c byte) (byte, bool) {
	switch {
	case c >= '0' && c <= '9':
		return c - '0', true
	case c >= 'a' && c <= 'f':
		return c - 'a' + 10, true
	case c >= 'A' && c <= 'F':
		return c - 'A' + 10, true
	default:
		return 0, false
	}
}
