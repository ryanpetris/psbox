package objects

// Desktop entry rewrite and host file ownership.

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"unicode"

	"petris.dev/psbox/internal/config"
)

const (
	GeneratorMarker = "# @generator psbox"
	ClientName      = "psbox"
)

// RewriteDesktop rewrites a desktop-entry body for application.
func RewriteDesktop(entry *config.DesktopEntry, application string, paths config.Paths) (string, error) {
	value := entry.Value
	if value == "" {
		var err error
		value, err = readDesktopTemplate(entry, paths)
		if err != nil {
			return "", err
		}
	}

	var out []string
	currentSection := ""
	mainWroteDBus := false
	inMain := false

	flushMain := func() {
		if inMain && !mainWroteDBus {
			out = append(out, "DBusActivatable=false")
			mainWroteDBus = true
		}
	}

	for _, line := range splitKeep(value) {
		if strings.HasPrefix(line, "[") {
			flushMain()
			end := strings.Index(line, "]")
			if end > 1 {
				currentSection = line[1:end]
			} else {
				currentSection = ""
			}
			inMain = currentSection == "Desktop Entry"
			out = append(out, line)
			for _, item := range entry.Inject {
				if item.Section != "" && !matchOptional(item.Section, currentSection) {
					continue
				}
				out = append(out, item.Value)
			}
			continue
		}

		if currentSection == "" || strings.HasPrefix(line, "#") || !strings.Contains(line, "=") {
			out = append(out, line)
			continue
		}

		field, rawValue, ok := strings.Cut(line, "=")
		if !ok {
			out = append(out, line)
			continue
		}

		fieldValue := rawValue
		for _, override := range entry.Overrides {
			if override.Pattern == "" {
				continue
			}
			if override.Section != "" && !matchOptional(override.Section, currentSection) {
				continue
			}
			if override.Field != "" && !matchOptional(override.Field, field) {
				continue
			}
			re, err := regexp.Compile(override.Pattern)
			if err != nil {
				return "", fmt.Errorf("override pattern in %s: %w", entry.Name, err)
			}
			fieldValue = re.ReplaceAllString(fieldValue, override.Replacement)
		}

		switch field {
		case "TryExec":
			continue
		case "Exec":
			fieldValue = rewriteExec(fieldValue, application, paths)
		case "Icon":
			fieldValue = paths.ReplaceTokens(fieldValue)
		case "DBusActivatable":
			if inMain {
				fieldValue = "false"
				mainWroteDBus = true
			}
		}

		out = append(out, field+"="+fieldValue)
	}
	flushMain()
	if len(out) == 0 || out[len(out)-1] != "" {
		out = append(out, "")
	}
	return strings.Join(out, "\n"), nil
}

func rewriteExec(value, application string, paths config.Paths) string {
	tokens, spans := splitExec(value)
	idx := 0
	for idx < len(tokens) {
		tok := tokens[idx]
		if tok == "env" || isEnvAssign(tok) {
			idx++
			continue
		}
		break
	}
	if idx >= len(tokens) {
		replaced := paths.ReplaceTokens(value)
		return launchExec(application) + " --command -- " + replaced
	}
	b := tokens[idx]
	if filepath.Base(b) == b {
		remainder := ""
		if spans[idx].end < len(value) {
			remainder = value[spans[idx].end:]
		}
		return launchExec(application) + " --" + paths.ReplaceTokens(remainder)
	}
	return launchExec(application) + " --command -- " + paths.ReplaceTokens(value)
}

func launchExec(application string) string {
	return ClientName + " launch " + application
}

type span struct {
	start int
	end   int
}

func splitExec(value string) ([]string, []span) {
	var tokens []string
	var spans []span
	i := 0
	for i < len(value) {
		for i < len(value) && unicode.IsSpace(rune(value[i])) {
			i++
		}
		if i >= len(value) {
			break
		}
		start := i
		var b strings.Builder
		for i < len(value) && !unicode.IsSpace(rune(value[i])) {
			switch value[i] {
			case '\\':
				if i+1 < len(value) {
					i++
					b.WriteByte(value[i])
					i++
					continue
				}
				i++
			case '\'':
				i++
				for i < len(value) && value[i] != '\'' {
					b.WriteByte(value[i])
					i++
				}
				if i < len(value) {
					i++
				}
			case '"':
				i++
				for i < len(value) && value[i] != '"' {
					if value[i] == '\\' && i+1 < len(value) {
						i++
					}
					b.WriteByte(value[i])
					i++
				}
				if i < len(value) {
					i++
				}
			default:
				b.WriteByte(value[i])
				i++
			}
		}
		tokens = append(tokens, b.String())
		spans = append(spans, span{start: start, end: i})
	}
	return tokens, spans
}

func isEnvAssign(tok string) bool {
	eq := strings.IndexByte(tok, '=')
	if eq <= 0 {
		return false
	}
	return config.CheckEnvKey(tok[:eq]) == nil
}

func matchOptional(pattern, value string) bool {
	re, err := regexp.Compile(pattern)
	if err != nil {
		return false
	}
	return re.MatchString(value)
}

func splitKeep(value string) []string {
	value = strings.ReplaceAll(value, "\r\n", "\n")
	return strings.Split(value, "\n")
}

func readDesktopTemplate(entry *config.DesktopEntry, paths config.Paths) (string, error) {
	var dirs []string
	switch entry.Kind {
	case config.KindFreedesktopEntry:
		for _, dir := range paths.DataDirs {
			dirs = append(dirs, filepath.Join(dir, "applications"))
		}
	case config.KindFreedesktopAutostartEntry:
		for _, dir := range paths.ConfigDirs {
			dirs = append(dirs, filepath.Join(dir, "autostart"))
		}
	default:
		return "", fmt.Errorf("invalid desktop entry kind %q", entry.Kind)
	}
	filename := entry.Name + ".desktop"
	for _, dir := range dirs {
		path := filepath.Join(dir, filename)
		data, err := os.ReadFile(path)
		if err != nil {
			if os.IsNotExist(err) {
				continue
			}
			return "", err
		}
		return string(data), nil
	}
	return "", fmt.Errorf("could not find desktop entry %q in %s", entry.Name, strings.Join(dirs, ", "))
}

func ownedByGenerator(path string) (bool, error) {
	f, err := os.Open(path)
	if err != nil {
		return false, err
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		if sc.Text() == GeneratorMarker {
			return true, nil
		}
	}
	return false, sc.Err()
}

func installBody(body string) string {
	if strings.HasPrefix(body, GeneratorMarker+"\n") {
		return body
	}
	return GeneratorMarker + "\n" + body
}

func squishHome(path, home string) string {
	if path == home || path == home+"/" {
		return "~"
	}
	if strings.HasPrefix(path, home+"/") {
		return "~" + path[len(home):]
	}
	return path
}
