package objects

// Install, cleanup, list, and render operations.

import (
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"text/tabwriter"

	"petris.dev/psbox/internal/config"
)

// Service implements `psbox objects`.
type Service struct {
	loader *config.Loader
	log    *slog.Logger
}

// NewService returns an objects service.
func NewService(loader *config.Loader, log *slog.Logger) *Service {
	return &Service{loader: loader, log: log}
}

// Install writes configured desktop files and removes orphan generator files.
func (s *Service) Install(paths config.Paths) error {
	col, err := s.loader.LoadPath(paths.ObjectPath, paths)
	if err != nil {
		return err
	}
	paths = paths.WithCollection(col)
	if err := s.requireSandbox(col); err != nil {
		return err
	}

	good := map[string]struct{}{}
	var missing []string

	for _, entry := range allEntries(col) {
		dir := desktopDir(entry.Kind, paths)
		path := filepath.Join(dir, entry.Name+".desktop")
		body, err := RewriteDesktop(entry, entry.Application, paths)
		if err != nil {
			if errors.Is(err, errTemplateMissing) {
				missing = append(missing, squishHome(path, paths.Home))
				continue
			}
			return fmt.Errorf("rewrite desktop entry %s: %w", entry.Name, err)
		}
		good[path] = struct{}{}
		if err := s.writeIfChanged(path, installBody(body), paths.Home); err != nil {
			return err
		}
	}

	if err := s.removeOrphans(paths, good); err != nil {
		return err
	}

	for _, path := range missing {
		s.log.Warn(fmt.Sprintf("Entry defined but not installed: %s", path), "path", path)
	}

	return nil
}

func (s *Service) requireSandbox(col *config.Collection) error {
	names := make([]string, 0, len(col.SandboxErrors))
	for name := range col.SandboxErrors {
		names = append(names, name)
	}
	sort.Strings(names)
	if len(names) == 0 {
		return nil
	}
	return col.SandboxErrors[names[0]]
}

// Cleanup deletes desktop files marked # @generator psbox.
func (s *Service) Cleanup(paths config.Paths) error {
	return s.removeOrphans(paths, nil)
}

// List writes object names to stdout.
func (s *Service) List(paths config.Paths, kind string, quiet bool, stdout io.Writer) error {
	col, err := s.loader.LoadPath(paths.ObjectPath, paths)
	if err != nil {
		return err
	}
	switch kind {
	case "applications":
		for _, name := range col.ListApplications() {
			if _, err := fmt.Fprintln(stdout, name); err != nil {
				return err
			}
		}
	case "desktop":
		return listEntries(stdout, col, col.FreedesktopEntries, quiet)
	case "autostart":
		return listEntries(stdout, col, col.FreedesktopAutostartEntries, quiet)
	default:
		return fmt.Errorf("unknown list kind %q", kind)
	}
	return nil
}

func listEntries(w io.Writer, col *config.Collection, entries map[string]*config.DesktopEntry, quiet bool) error {
	if quiet {
		names := make([]string, 0, len(entries))
		for name := range entries {
			names = append(names, name)
		}
		sort.Strings(names)
		for _, name := range names {
			if _, err := fmt.Fprintln(w, name); err != nil {
				return err
			}
		}
		return nil
	}
	tw := tabwriter.NewWriter(w, 0, 0, 2, ' ', 0)
	if _, err := fmt.Fprintln(tw, "APPLICATION\tENTRY"); err != nil {
		return err
	}
	for _, app := range col.ListApplications() {
		var names []string
		for name, entry := range entries {
			if entry.Application == app {
				names = append(names, name)
			}
		}
		sort.Strings(names)
		for _, name := range names {
			if _, err := fmt.Fprintf(tw, "%s\t%s\n", app, name); err != nil {
				return err
			}
		}
	}
	return tw.Flush()
}

// Render writes rewritten desktop files for an application to stdout.
func (s *Service) Render(paths config.Paths, application string, stdout io.Writer) error {
	col, err := s.loader.LoadPath(paths.ObjectPath, paths)
	if err != nil {
		return err
	}
	paths = paths.WithCollection(col)
	if _, err := col.RequireApplication(application); err != nil {
		return err
	}
	first := true
	for _, entry := range col.DesktopEntries(application) {
		body, err := RewriteDesktop(entry, entry.Application, paths)
		if err != nil {
			body = "# " + err.Error()
		}
		if !first {
			if _, err := fmt.Fprintln(stdout); err != nil {
				return err
			}
		}
		first = false
		dir := desktopDir(entry.Kind, paths)
		path := filepath.Join(dir, entry.Name+".desktop")
		if _, err := fmt.Fprintf(stdout, "# %s\n", squishHome(path, paths.Home)); err != nil {
			return err
		}
		if _, err := fmt.Fprint(stdout, body); err != nil {
			return err
		}
		if !strings.HasSuffix(body, "\n") {
			if _, err := fmt.Fprintln(stdout); err != nil {
				return err
			}
		}
	}
	return nil
}

func allEntries(col *config.Collection) []*config.DesktopEntry {
	var out []*config.DesktopEntry
	for _, entry := range col.FreedesktopEntries {
		out = append(out, entry)
	}
	for _, entry := range col.FreedesktopAutostartEntries {
		out = append(out, entry)
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].Kind != out[j].Kind {
			return out[i].Kind < out[j].Kind
		}
		return out[i].Name < out[j].Name
	})
	return out
}

func desktopDir(kind string, paths config.Paths) string {
	switch kind {
	case config.KindFreedesktopAutostartEntry:
		return filepath.Join(paths.ConfigHome, "autostart")
	default:
		return filepath.Join(paths.DataHome, "applications")
	}
}

func (s *Service) writeIfChanged(path, body, home string) error {
	if current, err := os.ReadFile(path); err == nil && string(current) == body {
		return nil
	} else if err == nil {
		s.log.Info(fmt.Sprintf("Updating %s...", squishHome(path, home)), "path", path)
	} else if os.IsNotExist(err) {
		s.log.Info(fmt.Sprintf("Creating %s...", squishHome(path, home)), "path", path)
	} else {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	return os.WriteFile(path, []byte(body), 0o644)
}

func (s *Service) removeOrphans(paths config.Paths, keep map[string]struct{}) error {
	dirs := []string{
		filepath.Join(paths.DataHome, "applications"),
		filepath.Join(paths.ConfigHome, "autostart"),
	}
	for _, dir := range dirs {
		entries, err := os.ReadDir(dir)
		if err != nil {
			if os.IsNotExist(err) {
				continue
			}
			return err
		}
		for _, entry := range entries {
			if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".desktop") {
				continue
			}
			path := filepath.Join(dir, entry.Name())
			if _, ok := keep[path]; ok {
				continue
			}
			owned, err := ownedByGenerator(path)
			if err != nil {
				return err
			}
			if !owned {
				continue
			}
			s.log.Info(fmt.Sprintf("Removing %s...", squishHome(path, paths.Home)), "path", path)
			if err := os.Remove(path); err != nil {
				return err
			}
		}
	}
	return nil
}
