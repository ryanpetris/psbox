package instance

// Sandbox mimeapps defaults so GIO http(s) opens go through psboxa.

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

const (
	hostURLDesktopID = "psbox-open-url.desktop"
	mimeHTTP         = "x-scheme-handler/http"
	mimeHTTPS        = "x-scheme-handler/https"
	defaultAppsSec   = "Default Applications"
)

func ensureHostURLHandler(env map[string]string) error {
	dataHome := xdgDataHome(env)
	configHome := xdgConfigHome(env)
	if dataHome == "" || configHome == "" {
		return fmt.Errorf("no XDG data or config home")
	}

	appDir := filepath.Join(dataHome, "applications")
	if err := os.MkdirAll(appDir, 0o755); err != nil {
		return err
	}
	if err := os.MkdirAll(configHome, 0o755); err != nil {
		return err
	}
	if err := writeHostURLDesktop(filepath.Join(appDir, hostURLDesktopID)); err != nil {
		return err
	}
	return updateMimeappsList(filepath.Join(configHome, "mimeapps.list"))
}

func writeHostURLDesktop(path string) error {
	exe := "psboxa"
	if p, err := os.Executable(); err == nil && p != "" {
		exe = p
	}
	body := "[Desktop Entry]\n" +
		"Type=Application\n" +
		"Name=Open URL on host\n" +
		"Exec=" + quoteDesktopExec(exe) + " xdg-open %u\n" +
		"NoDisplay=true\n" +
		"MimeType=x-scheme-handler/http;x-scheme-handler/https;\n"
	return os.WriteFile(path, []byte(body), 0o644)
}

func removeHostURLHandler(env map[string]string) error {
	dataHome := xdgDataHome(env)
	configHome := xdgConfigHome(env)
	if dataHome == "" || configHome == "" {
		return fmt.Errorf("no XDG data or config home")
	}

	desktop := filepath.Join(dataHome, "applications", hostURLDesktopID)
	if err := os.Remove(desktop); err != nil && !os.IsNotExist(err) {
		return err
	}
	return stripMimeappsList(filepath.Join(configHome, "mimeapps.list"))
}

func updateMimeappsList(path string) error {
	var content string
	data, err := os.ReadFile(path)
	if err == nil {
		content = string(data)
	} else if !os.IsNotExist(err) {
		return err
	}

	updated, changed := mergeMimeappsDefaults(content, map[string]string{
		mimeHTTP:  hostURLDesktopID,
		mimeHTTPS: hostURLDesktopID,
	})
	if !changed {
		return nil
	}
	return os.WriteFile(path, []byte(updated), 0o644)
}

func stripMimeappsList(path string) error {
	data, err := os.ReadFile(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}
	updated, changed := stripMimeappsDefaults(string(data), map[string]string{
		mimeHTTP:  hostURLDesktopID,
		mimeHTTPS: hostURLDesktopID,
	})
	if !changed {
		return nil
	}
	return os.WriteFile(path, []byte(updated), 0o644)
}

func mergeMimeappsDefaults(content string, keys map[string]string) (string, bool) {
	if len(keys) == 0 {
		return content, false
	}

	needed := make(map[string]string, len(keys))
	for k, v := range keys {
		needed[k] = v
	}

	raw := strings.Split(strings.ReplaceAll(content, "\r\n", "\n"), "\n")
	// Split leaves a trailing empty element when content ends with \n.
	// Keep lines as written and rebuild with \n.
	if len(raw) == 1 && raw[0] == "" && content == "" {
		raw = nil
	}

	var out []string
	section := ""
	inDefaults := false
	changed := content == ""
	wroteSection := false

	flushMissing := func() {
		if !inDefaults {
			return
		}
		for _, k := range sortedMimeKeys(needed) {
			out = append(out, k+"="+needed[k])
			delete(needed, k)
			changed = true
		}
	}

	for _, line := range raw {
		trim := strings.TrimSpace(line)
		if strings.HasPrefix(trim, "[") && strings.HasSuffix(trim, "]") && !strings.HasPrefix(trim, "#") {
			flushMissing()
			section = strings.TrimSpace(trim[1 : len(trim)-1])
			inDefaults = section == defaultAppsSec
			if inDefaults {
				wroteSection = true
			}
			out = append(out, line)
			continue
		}
		if inDefaults {
			key, val, ok := splitMimeappsKey(trim)
			if ok {
				if want, need := needed[key]; need {
					delete(needed, key)
					if mimeappsDesktopID(val) == want {
						out = append(out, line)
						continue
					}
					out = append(out, key+"="+want)
					changed = true
					continue
				}
			}
		}
		out = append(out, line)
	}
	flushMissing()

	if len(needed) > 0 {
		if len(out) > 0 && out[len(out)-1] != "" {
			out = append(out, "")
		}
		if !wroteSection {
			out = append(out, "["+defaultAppsSec+"]")
		}
		for _, k := range sortedMimeKeys(needed) {
			out = append(out, k+"="+needed[k])
		}
		changed = true
	}

	result := strings.Join(out, "\n")
	if content != "" && !strings.HasSuffix(content, "\n") && !strings.HasSuffix(result, "\n") {
		// keep original lack of trailing newline unless we appended
	} else if !strings.HasSuffix(result, "\n") {
		result += "\n"
	}
	return result, changed
}

func stripMimeappsDefaults(content string, keys map[string]string) (string, bool) {
	if content == "" || len(keys) == 0 {
		return content, false
	}

	raw := strings.Split(strings.ReplaceAll(content, "\r\n", "\n"), "\n")
	var out []string
	section := ""
	changed := false

	for _, line := range raw {
		trim := strings.TrimSpace(line)
		if strings.HasPrefix(trim, "[") && strings.HasSuffix(trim, "]") && !strings.HasPrefix(trim, "#") {
			section = strings.TrimSpace(trim[1 : len(trim)-1])
			out = append(out, line)
			continue
		}
		if section == defaultAppsSec {
			key, val, ok := splitMimeappsKey(trim)
			if ok {
				if want, need := keys[key]; need && mimeappsDesktopID(val) == want {
					changed = true
					continue
				}
			}
		}
		out = append(out, line)
	}
	if !changed {
		return content, false
	}

	result := strings.Join(out, "\n")
	if content != "" && !strings.HasSuffix(content, "\n") && !strings.HasSuffix(result, "\n") {
		// keep original lack of trailing newline
	} else if result != "" && !strings.HasSuffix(result, "\n") {
		result += "\n"
	}
	return result, true
}

func sortedMimeKeys(m map[string]string) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}

func splitMimeappsKey(line string) (key, val string, ok bool) {
	if line == "" || strings.HasPrefix(line, "#") {
		return "", "", false
	}
	i := strings.IndexByte(line, '=')
	if i <= 0 {
		return "", "", false
	}
	key = strings.TrimSpace(line[:i])
	val = strings.TrimSpace(line[i+1:])
	if key == "" {
		return "", "", false
	}
	return key, val, true
}

func mimeappsDesktopID(val string) string {
	val = strings.TrimSpace(val)
	val = strings.TrimSuffix(val, ";")
	return strings.TrimSpace(val)
}

func xdgConfigHome(env map[string]string) string {
	if v := env["XDG_CONFIG_HOME"]; v != "" {
		return v
	}
	if home := env["HOME"]; home != "" {
		return filepath.Join(home, ".config")
	}
	return ""
}

func xdgDataHome(env map[string]string) string {
	if v := env["XDG_DATA_HOME"]; v != "" {
		return v
	}
	if home := env["HOME"]; home != "" {
		return filepath.Join(home, ".local", "share")
	}
	return ""
}

func quoteDesktopExec(path string) string {
	if strings.ContainsAny(path, " \t\"'\\") {
		path = strings.ReplaceAll(path, `\`, `\\`)
		path = strings.ReplaceAll(path, `"`, `\"`)
		return `"` + path + `"`
	}
	return path
}
