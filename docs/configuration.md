# Configuration

psbox reads a directory of YAML documents. Default path:
`$XDG_CONFIG_HOME/psbox/objects`. Every `*.yaml` file under that
directory is loaded, including nested files, in sorted path order.

Documents are `version: v1`. Unknown kinds and decode errors are
skipped with a warning on stderr. A YAML syntax error stops reading
the rest of that file after one warning. A missing objects directory
loads as an empty collection.

Names (applications, instances, desktop entries) match
`^[A-Za-z0-9._][A-Za-z0-9._-]*$`. They must not start with `-` and must
not contain `/` or `:`.

## Kinds

| `kind` | Role |
| --- | --- |
| `Application` | Named program and sandbox options |
| `FreedesktopEntry` | Desktop file installed to `~/.local/share/applications` |
| `FreedesktopAutostartEntry` | Desktop file installed to `~/.config/autostart` |
| `Options` | Collection-wide `sandbox_root` |

One file may contain several documents, separated by `---`.

When a file contains exactly one `Application` and `metadata.name` is
omitted, the name is the file stem. Desktop entries in that file
inherit the application name when their own `metadata.name` or
`spec.application` is omitted.

`metadata.type` is a closed application type. Unset keys take that
type's defaults; explicit `spec.options` always win. Unknown types are
skipped with a warning.

| Type | Fills when unset |
| --- | --- |
| `browser` | `dbus: private`, `host-urls: false`, `downloads: true` |

Duplicate application or desktop names: the last definition wins, with
a warning.

## Application

```yaml
version: v1
kind: Application
metadata:
  name: firefox
  type: browser
spec:
  exec: firefox
  default-args:
    - --new-tab
  env:
    MOZ_ENABLE_WAYLAND: "1"
```

### `spec.exec`

Command tokens. A string is a single token. A list is several. If
omitted, `exec` is the application name (looked up on `PATH` inside
the sandbox).

### `spec.default-args`

Appended when the launch has no extra arguments. Extra CLI arguments
replace them.

### `spec.env`

Merged onto the launching process environment at spawn time. Keys must
be legal environment names (letter or `_`, then letters, digits, `_`).
Values must not contain NUL.

### `spec.bwrap-args`

Extra `bwrap` tokens after the composed flags and before the command.
Each item is a string or a list of strings. A two-token bind
(`--bind`, `--ro-bind`, `--dev-bind`, and the `-try` forms) with no
destination gets the source copied as the destination.

`:root:` expands to the sandbox root. `:home:` and a leading `~`
expand to the host home (not the private home).

### `spec.options`

| Key | Default | Meaning |
| --- | --- | --- |
| `home` | `$PSBOX_ROOT/<sandbox>` | Private home path, `:tmpfs:`, or `:none:` |
| `audio` | `true` | Pulse/PipeWire and `/dev/snd` |
| `video` | `true` | `/dev/video*` and `/dev/v4l` |
| `dbus` | host | `true`/`host`, `private`, or `false`/`off` |
| `display` | `true` | Wayland, X11, DRI |
| `fontconfig` | `true` | `/var/cache/fontconfig` |
| `isolation` | `instance` | `oneshot`, `instance`, or `instance:<name>` |
| `host-urls` | `true` | Forward in-sandbox `http`/`https` to the host |
| `cache` | `tmpfs` | `tmpfs` mounts `$HOME/.cache`; `home` keeps it in the private home |
| `downloads` | `false` | Bind the host `Downloads` directory into `$HOME/Downloads` |
| `sandbox` | unset | Name of the application that owns the sandbox |

Boolean options that are omitted or `true` do not change the config
hash. Setting `false` does. `cache: tmpfs` (the default) is omitted
from the hash; `cache: home` is included.

`dbus: private` binds neither the host session bus nor the system bus
(`/run/dbus`). Instance isolation starts `dbus-daemon --session --nofork`
inside the sandbox. Oneshot isolation wraps the workload with
`dbus-launch`.

`dbus: false` also binds neither host bus, and starts no in-sandbox
session bus.

`cache: tmpfs` is the default whenever the sandbox has a real home
bind. It is skipped when `home` is `:tmpfs:` or `:none:`. `cache:
home` leaves `~/.cache` on the private home.

`downloads: true` binds the host `~/Downloads` directory. It is
skipped when `home` is `:none:`. `downloads: true` is hashed;
omitted/`false` is not.

`host-urls: false` is for browsers and other apps that should own
`http`/`https` themselves. It drops the `xdg-open` `PATH` shim, does
not claim those schemes in the sandbox `mimeapps.list`, and removes a
leftover `psbox-open-url` handler from an earlier spawn. See
[urls.md](urls.md). Oneshot isolation never forwards.

## Referrers

```yaml
version: v1
kind: Application
metadata:
  name: firefox-work
spec:
  exec: firefox
  default-args: ["--profile", "work"]
  options:
    sandbox: firefox
```

Launch identity is the **target** (`firefox`), so
`psbox launch firefox-work` joins `firefox/default`. The referrer may set
`exec`, `default-args`, and `env` only. Setting home, audio, video,
dbus, display, fontconfig, isolation, `host-urls`, `cache`,
`downloads`, `metadata.type`, or `bwrap-args` on a referrer is a
collection error: the referrer is omitted from lists and rejected at
launch, install, and render.

`sandbox:` is one hop. A target that itself sets `sandbox:` is an
error. A missing target is an error.

## Desktop entries

```yaml
version: v1
kind: FreedesktopEntry
metadata:
  name: firefox
spec:
  application: firefox
  # value: |
  #   [Desktop Entry]
  #   ...
  overrides:
    - field: Name
      pattern: ".*"
      replacement: "Firefox (Sandbox)"
  inject:
    - section: "Desktop Entry"
      value: "StartupWMClass=firefox"
```

If `spec.value` is omitted, `psbox objects install` reads
`$XDG_DATA_DIRS/applications/<name>.desktop` (autostart:
`$XDG_CONFIG_DIRS/autostart/<name>.desktop`).

Rewrite rules:

- `Exec=` becomes `psbox launch <application> -- …` when the binary is a
  bare name, or `psbox launch <application> --command -- …` when it is a path
- `TryExec=` is removed
- `DBusActivatable=` in `[Desktop Entry]` is set to `false`
- `Icon=` expands `:root:`, `:home:`, and `~`

Installed files start with `# @generator psbox`. `psbox objects install`
updates those files and deletes generator-owned orphans that are no
longer configured. It does not delete desktop files it did not write.

## Collection options

```yaml
version: v1
kind: Options
spec:
  sandbox_root: ~/Sandbox
```

`sandbox_root` sets `$PSBOX_ROOT` for the collection. A leading `~` is
expanded using the host home. Later `Options` documents replace earlier
ones.

## Paths and environment

| Variable | Default | Use |
| --- | --- | --- |
| `PSBOX_OBJECT_PATH` | `$XDG_CONFIG_HOME/psbox/objects` | Objects directory |
| `PSBOX_ROOT` | `$HOME/Sandbox` | Default private-home parent |
| `PSBOX_AGENT_BIN` | `/usr/bin/psboxa` | In-sandbox agent binary |
| `XDG_CONFIG_HOME` | `~/.config` | Objects default, desktop install |
| `XDG_DATA_HOME` | `~/.local/share` | Desktop install |
| `XDG_RUNTIME_DIR` | `/run/user/<uid>` | Instance sockets, sandbox tmpfs |
| `PATH` | `/usr/local/bin:/usr/bin` | Used when the spawn environment has no `PATH` |
| `WAYLAND_DISPLAY` | `wayland-0` | Display socket bind |
| `XAUTHORITY` | unset | X11 cookie bind |
| `PIPEWIRE_CORE` | `pipewire-0` | PipeWire socket name |

`--objects` on `psbox launch` and `psbox objects` overrides the objects
path for that process. Combined with instance isolation it is a hard
error when the path is not the process default: the running daemon
already loaded its own objects.

## Tokens

In `home`, `bwrap-args`, and desktop `Icon`/`Exec` rewrite:

- `:root:` is `PSBOX_ROOT`
- `:home:` and `~/` are the **host** home
