package config

// YAML object loading from a directory of v1 documents.

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"gopkg.in/yaml.v3"
)

// Loader reads object YAML from a directory.
type Loader struct {
	log *slog.Logger
}

// NewLoader returns a Loader that writes object-load warnings to log.
func NewLoader(log *slog.Logger) *Loader {
	if log == nil {
		log = slog.Default()
	}
	return &Loader{log: log}
}

// LoadPath loads every *.yaml file under dir, sorted by path.
func (l *Loader) LoadPath(dir string, paths Paths) (*Collection, error) {
	col := newCollection(paths)
	entries, err := listYAML(dir)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return col, nil
		}
		return nil, err
	}
	for _, path := range entries {
		objs, ferr := l.loadFile(path)
		if ferr != nil {
			return nil, ferr
		}
		for _, obj := range objs {
			l.add(col, obj)
		}
	}
	col.resolveSandbox()
	return col, nil
}

func newCollection(paths Paths) *Collection {
	return &Collection{
		Root:                        paths.Root,
		ObjectPath:                  paths.ObjectPath,
		Applications:                map[string]*Application{},
		FreedesktopEntries:          map[string]*DesktopEntry{},
		FreedesktopAutostartEntries: map[string]*DesktopEntry{},
		SandboxErrors:               map[string]error{},
	}
}

func listYAML(dir string) ([]string, error) {
	var files []string
	err := filepath.WalkDir(dir, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			return nil
		}
		if strings.HasSuffix(d.Name(), ".yaml") {
			files = append(files, path)
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	sort.Strings(files)
	return files, nil
}

type rawObject struct {
	Version  string    `yaml:"version"`
	Kind     string    `yaml:"kind"`
	Metadata rawMeta   `yaml:"metadata"`
	Spec     yaml.Node `yaml:"spec"`
}

type rawMeta struct {
	Name string `yaml:"name"`
	Type string `yaml:"type"`
}

func (l *Loader) loadFile(path string) ([]any, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	dec := yaml.NewDecoder(f)
	var raws []rawObject
	for {
		var raw rawObject
		err := dec.Decode(&raw)
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			// A syntax error leaves the decoder at the same
			// position; continuing would warn forever.
			l.skipObject(path, err)
			break
		}
		raws = append(raws, raw)
	}

	var objs []any
	for i := range raws {
		obj, err := parseObject(path, &raws[i])
		if err != nil {
			l.skipObject(path, err)
			continue
		}
		objs = append(objs, obj)
	}
	return fixupSameFile(path, objs), nil
}

func parseObject(path string, raw *rawObject) (any, error) {
	if raw.Version == "" && raw.Kind == "" {
		return nil, fmt.Errorf("missing version and kind")
	}
	if raw.Version == "" {
		return nil, fmt.Errorf("missing version")
	}
	if raw.Kind == "" {
		return nil, fmt.Errorf("missing kind")
	}
	if raw.Version != VersionV1 {
		return nil, fmt.Errorf("unsupported version %q for kind %s", raw.Version, raw.Kind)
	}

	switch raw.Kind {
	case KindApplication:
		return parseApplication(path, raw)
	case KindFreedesktopEntry, KindFreedesktopAutostartEntry:
		return parseDesktop(path, raw)
	case KindOptions:
		return parseOptions(path, raw)
	default:
		return nil, fmt.Errorf("unknown kind %q", raw.Kind)
	}
}

func parseApplication(path string, raw *rawObject) (*Application, error) {
	spec, err := parseApplicationSpec(&raw.Spec)
	if err != nil {
		return nil, err
	}
	name := raw.Metadata.Name
	if name != "" {
		if err := CheckName("application", name); err != nil {
			return nil, err
		}
	}
	if err := ValidateEnv(spec.Env); err != nil {
		return nil, err
	}
	if spec.Options.Isolation != nil {
		if _, err := ParseIsolation(*spec.Options.Isolation); err != nil {
			return nil, err
		}
	}
	if spec.Options.Sandbox != nil && *spec.Options.Sandbox != "" {
		if err := CheckName("sandbox", *spec.Options.Sandbox); err != nil {
			return nil, err
		}
	}
	typ, err := parseApplicationType(raw.Metadata.Type)
	if err != nil {
		return nil, err
	}
	app := &Application{File: path, Name: name, Type: typ, Spec: spec}
	if sandboxTarget(app) == "" {
		applyTypeDefaults(app)
	}
	return app, nil
}

func parseApplicationSpec(node *yaml.Node) (ApplicationSpec, error) {
	var spec ApplicationSpec
	if node == nil || node.Kind == 0 {
		return spec, nil
	}
	var raw struct {
		Exec        yaml.Node            `yaml:"exec"`
		DefaultArgs yaml.Node            `yaml:"default-args"`
		Env         map[string]string    `yaml:"env"`
		BwrapArgs   []yaml.Node          `yaml:"bwrap-args"`
		Options     map[string]yaml.Node `yaml:"options"`
	}
	if err := node.Decode(&raw); err != nil {
		return spec, err
	}
	exec, err := decodeStringList(&raw.Exec)
	if err != nil {
		return spec, fmt.Errorf("exec: %w", err)
	}
	defaults, err := decodeStringList(&raw.DefaultArgs)
	if err != nil {
		return spec, fmt.Errorf("default-args: %w", err)
	}
	bwrapArgs, err := decodeBwrapArgs(raw.BwrapArgs)
	if err != nil {
		return spec, err
	}
	opts, err := decodeApplicationOptions(raw.Options)
	if err != nil {
		return spec, err
	}
	spec.Exec = exec
	spec.DefaultArgs = defaults
	spec.Env = raw.Env
	spec.BwrapArgs = bwrapArgs
	spec.Options = opts
	return spec, nil
}

func decodeApplicationOptions(raw map[string]yaml.Node) (ApplicationOptions, error) {
	var opts ApplicationOptions
	if raw == nil {
		return opts, nil
	}
	for key, node := range raw {
		switch key {
		case "home":
			var v string
			if err := node.Decode(&v); err != nil {
				return opts, fmt.Errorf("options.home: %w", err)
			}
			opts.Home = &v
		case "audio":
			v, err := decodeBool(node, "options.audio")
			if err != nil {
				return opts, err
			}
			opts.Audio = &v
		case "video":
			v, err := decodeBool(node, "options.video")
			if err != nil {
				return opts, err
			}
			opts.Video = &v
		case "dbus":
			v, err := decodeDBus(node)
			if err != nil {
				return opts, err
			}
			opts.DBus = &v
		case "display":
			v, err := decodeBool(node, "options.display")
			if err != nil {
				return opts, err
			}
			opts.Display = &v
		case "fontconfig":
			v, err := decodeBool(node, "options.fontconfig")
			if err != nil {
				return opts, err
			}
			opts.Fontconfig = &v
		case "isolation":
			var v string
			if err := node.Decode(&v); err != nil {
				return opts, fmt.Errorf("options.isolation: %w", err)
			}
			opts.Isolation = &v
		case "sandbox":
			var v string
			if err := node.Decode(&v); err != nil {
				return opts, fmt.Errorf("options.sandbox: %w", err)
			}
			opts.Sandbox = &v
		case "host-urls":
			v, err := decodeBool(node, "options.host-urls")
			if err != nil {
				return opts, err
			}
			opts.HostURLs = &v
		case "cache":
			v, err := decodeCache(node)
			if err != nil {
				return opts, err
			}
			opts.Cache = &v
		case "downloads":
			v, err := decodeBool(node, "options.downloads")
			if err != nil {
				return opts, err
			}
			opts.Downloads = &v
		default:
			return opts, fmt.Errorf("unknown options key %q", key)
		}
	}
	return opts, nil
}

func decodeDBus(node yaml.Node) (string, error) {
	var asBool bool
	if err := node.Decode(&asBool); err == nil {
		if asBool {
			return DBusHost, nil
		}
		return DBusOff, nil
	}
	var asString string
	if err := node.Decode(&asString); err != nil {
		return "", fmt.Errorf("options.dbus: %w", err)
	}
	switch asString {
	case "private":
		return DBusPrivate, nil
	case "true":
		return DBusHost, nil
	case "false":
		return DBusOff, nil
	default:
		return "", fmt.Errorf("invalid options.dbus %q", asString)
	}
}

func decodeCache(node yaml.Node) (string, error) {
	var v string
	if err := node.Decode(&v); err != nil {
		return "", fmt.Errorf("options.cache: %w", err)
	}
	switch v {
	case CacheTmpfs, CacheHome:
		return v, nil
	default:
		return "", fmt.Errorf("invalid options.cache %q", v)
	}
}

func decodeBool(node yaml.Node, field string) (bool, error) {
	var v bool
	if err := node.Decode(&v); err != nil {
		return false, fmt.Errorf("%s: %w", field, err)
	}
	return v, nil
}

func decodeStringList(node *yaml.Node) ([]string, error) {
	if node == nil || node.Kind == 0 || node.Tag == "!!null" {
		return nil, nil
	}
	if node.Kind == yaml.ScalarNode {
		var s string
		if err := node.Decode(&s); err != nil {
			return nil, err
		}
		if s == "" {
			return nil, nil
		}
		return []string{s}, nil
	}
	var list []string
	if err := node.Decode(&list); err != nil {
		return nil, err
	}
	return list, nil
}

func decodeBwrapArgs(nodes []yaml.Node) ([][]string, error) {
	var out [][]string
	for i, node := range nodes {
		if node.Kind == yaml.ScalarNode {
			var s string
			if err := node.Decode(&s); err != nil {
				return nil, fmt.Errorf("bwrap-args[%d]: %w", i, err)
			}
			out = append(out, []string{s})
			continue
		}
		var list []string
		if err := node.Decode(&list); err != nil {
			return nil, fmt.Errorf("bwrap-args[%d]: %w", i, err)
		}
		out = append(out, list)
	}
	return out, nil
}

func parseDesktop(path string, raw *rawObject) (*DesktopEntry, error) {
	var spec struct {
		Application string     `yaml:"application"`
		Value       string     `yaml:"value"`
		Overrides   []Override `yaml:"overrides"`
		Inject      []Inject   `yaml:"inject"`
	}
	if raw.Spec.Kind != 0 {
		if err := raw.Spec.Decode(&spec); err != nil {
			return nil, err
		}
	}
	name := raw.Metadata.Name
	if name != "" {
		if err := CheckName("desktop entry", name); err != nil {
			return nil, err
		}
	}
	if spec.Application != "" {
		if err := CheckName("application", spec.Application); err != nil {
			return nil, err
		}
	}
	return &DesktopEntry{
		File:        path,
		Name:        name,
		Kind:        raw.Kind,
		Application: spec.Application,
		Value:       spec.Value,
		Overrides:   spec.Overrides,
		Inject:      spec.Inject,
	}, nil
}

type optionsObject struct {
	SandboxRoot string
}

func parseOptions(path string, raw *rawObject) (*optionsObject, error) {
	var spec struct {
		SandboxRoot string `yaml:"sandbox_root"`
	}
	if raw.Spec.Kind != 0 {
		if err := raw.Spec.Decode(&spec); err != nil {
			return nil, err
		}
	}
	root := spec.SandboxRoot
	if root != "" {
		if expanded, err := expandOptionalHome(root); err == nil {
			root = expanded
		}
	}
	_ = path
	return &optionsObject{SandboxRoot: root}, nil
}

func expandOptionalHome(val string) (string, error) {
	if !strings.HasPrefix(val, "~") {
		return val, nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return val, err
	}
	return expandUser(val, home), nil
}

func fixupSameFile(path string, objs []any) []any {
	var apps []*Application
	for _, obj := range objs {
		if app, ok := obj.(*Application); ok {
			apps = append(apps, app)
		}
	}
	if len(apps) != 1 {
		return objs
	}
	app := apps[0]
	if app.Name == "" {
		base := filepath.Base(path)
		app.Name = strings.TrimSuffix(base, filepath.Ext(base))
		if err := CheckName("application", app.Name); err != nil {
			return objs
		}
	}
	for _, obj := range objs {
		entry, ok := obj.(*DesktopEntry)
		if !ok {
			continue
		}
		if entry.Name == "" {
			entry.Name = app.Name
		}
		if entry.Application == "" {
			entry.Application = app.Name
		}
	}
	return objs
}

func (l *Loader) skipObject(path string, err error) {
	l.log.Warn(fmt.Sprintf("skipping object %s: %v", path, err), "file", path, "error", err)
}

func (l *Loader) add(col *Collection, obj any) {
	switch v := obj.(type) {
	case *Application:
		if v.Name == "" {
			l.skipObject(v.File, fmt.Errorf("application name is empty"))
			return
		}
		if _, ok := col.Applications[v.Name]; ok {
			l.log.Warn(fmt.Sprintf("duplicate application definition %s (%s)", v.Name, v.File), "name", v.Name, "file", v.File)
		}
		col.Applications[v.Name] = v
	case *DesktopEntry:
		if v.Name == "" {
			l.skipObject(v.File, fmt.Errorf("desktop entry name is empty"))
			return
		}
		switch v.Kind {
		case KindFreedesktopEntry:
			if _, ok := col.FreedesktopEntries[v.Name]; ok {
				l.log.Warn(fmt.Sprintf("duplicate desktop entry definition %s (%s)", v.Name, v.File), "name", v.Name, "file", v.File)
			}
			col.FreedesktopEntries[v.Name] = v
		case KindFreedesktopAutostartEntry:
			if _, ok := col.FreedesktopAutostartEntries[v.Name]; ok {
				l.log.Warn(fmt.Sprintf("duplicate autostart entry definition %s (%s)", v.Name, v.File), "name", v.Name, "file", v.File)
			}
			col.FreedesktopAutostartEntries[v.Name] = v
		}
	case *optionsObject:
		if v.SandboxRoot != "" {
			col.Root = v.SandboxRoot
		}
	}
}
