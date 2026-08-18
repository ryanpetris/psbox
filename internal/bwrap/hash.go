package bwrap

// Canonical config and running hashes.

import (
	"encoding/hex"
	"encoding/json"
	"path/filepath"
	"sort"
	"strings"

	"golang.org/x/crypto/blake2b"

	"petris.dev/psbox/internal/config"
)

// Hashes is the pair of blake2b-512 digests used as a drift detector.
type Hashes struct {
	Config  []byte
	Running []byte
}

// Hex returns lowercase hex encodings of both hashes.
func (h Hashes) Hex() (configHash, runningHash string) {
	return hex.EncodeToString(h.Config), hex.EncodeToString(h.Running)
}

// ComputeHashes returns the config and running hashes for the sandbox target.
func ComputeHashes(app *config.Application, paths config.Paths, env Env) (Hashes, error) {
	configPayload, err := canonicalConfig(app, paths)
	if err != nil {
		return Hashes{}, err
	}
	configSum := blake2b512(configPayload)
	runningPayload, err := canonicalRunning(configSum, env, paths)
	if err != nil {
		return Hashes{}, err
	}
	return Hashes{
		Config:  configSum,
		Running: blake2b512(runningPayload),
	}, nil
}

func blake2b512(data []byte) []byte {
	sum := blake2b.Sum512(data)
	return sum[:]
}

func canonicalConfig(app *config.Application, paths config.Paths) ([]byte, error) {
	body := map[string]any{}
	opts := app.Spec.Options

	if opts.Home != nil {
		body["home"] = hashHome(*opts.Home, paths)
	}
	if v := falseIfNotDefault(opts.Audio); v != nil {
		body["audio"] = *v
	}
	if v := falseIfNotDefault(opts.Video); v != nil {
		body["video"] = *v
	}
	if opts.DBus != nil && *opts.DBus != config.DBusHost {
		body["dbus"] = *opts.DBus
	}
	if v := falseIfNotDefault(opts.Display); v != nil {
		body["display"] = *v
	}
	if v := falseIfNotDefault(opts.Fontconfig); v != nil {
		body["fontconfig"] = *v
	}
	if v := falseIfNotDefault(opts.HostURLs); v != nil {
		body["host-urls"] = *v
	}
	if opts.Cache != nil && *opts.Cache != "" && *opts.Cache != config.CacheTmpfs {
		body["cache"] = *opts.Cache
	}
	if opts.Downloads != nil && *opts.Downloads {
		body["downloads"] = true
	}
	if args := hashBwrapArgs(app.Spec.BwrapArgs, paths); len(args) > 0 {
		body["bwrap-args"] = args
	}

	return json.Marshal(body)
}

func hashHome(home string, paths config.Paths) string {
	if home == config.HomeNone || home == config.HomeTmpfs {
		return home
	}
	return hashReplace(home, paths)
}

func hashBwrapArgs(args [][]string, paths config.Paths) [][]string {
	if len(args) == 0 {
		return nil
	}
	out := make([][]string, len(args))
	for i, arg := range args {
		item := make([]string, len(arg))
		for j, part := range arg {
			item[j] = hashReplace(part, paths)
		}
		out[i] = item
	}
	return out
}

func hashReplace(val string, paths config.Paths) string {
	val = strings.ReplaceAll(val, config.HomeToken, paths.Home)
	return expandHomePrefix(val, paths.Home)
}

func expandHomePrefix(val, home string) string {
	if val == "~" {
		return home
	}
	if len(val) >= 2 && val[0] == '~' && val[1] == '/' {
		return filepath.Join(home, val[2:])
	}
	return val
}

func falseIfNotDefault(ptr *bool) *bool {
	if ptr == nil || *ptr {
		return nil
	}
	v := false
	return &v
}

func canonicalRunning(configHash []byte, env Env, paths config.Paths) ([]byte, error) {
	m := map[string]string{}
	wayland := env.WaylandDisplay
	if wayland == "" {
		wayland = config.DefaultWaylandDisplay
	}
	if wayland != config.DefaultWaylandDisplay {
		m["WAYLAND_DISPLAY"] = wayland
	}
	if env.Xauthority != "" {
		m["XAUTHORITY"] = env.Xauthority
	}
	pipewire := env.PipewireCore
	if pipewire == "" {
		pipewire = config.DefaultPipewireCore
	}
	if pipewire != config.DefaultPipewireCore {
		m["PIPEWIRE_CORE"] = pipewire
	}
	root := env.Root
	if root == "" {
		root = paths.Root
	}
	if root != defaultRoot(paths) {
		m["PSBOX_ROOT"] = root
	}
	objectPath := env.ObjectPath
	if objectPath == "" {
		objectPath = paths.ObjectPath
	}
	if objectPath != defaultObjectPath(paths) {
		m["PSBOX_OBJECT_PATH"] = objectPath
	}

	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	envObj := map[string]string{}
	for _, k := range keys {
		envObj[k] = m[k]
	}
	envJSON, err := json.Marshal(envObj)
	if err != nil {
		return nil, err
	}
	out := make([]byte, 0, len(configHash)+len(envJSON))
	out = append(out, configHash...)
	out = append(out, envJSON...)
	return out, nil
}

func defaultRoot(paths config.Paths) string {
	return filepath.Join(paths.Home, config.DefaultRootRelative)
}

func defaultObjectPath(paths config.Paths) string {
	return filepath.Join(paths.ConfigHome, config.DefaultObjectDirRelative)
}
