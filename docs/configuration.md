# Configuration

## Locations

| What | Path |
| --- | --- |
| Sandbox configs | `$XDG_CONFIG_HOME/psbox/sandboxes/<name>.toml` |
| Profiles | `$XDG_CONFIG_HOME/psbox/profiles/<name>.toml` |
| Sandbox root | `$PSBOX_ROOT`, default `~/Sandbox` |
| Private home | `<root>/<name>` |
| Other sandbox directories | `<root>/<name>.<type>`, such as `~/Sandbox/retroarch.bin` |
| Supervisor sockets | `$XDG_RUNTIME_DIR/psbox/<key>.sock` |

One TOML file per sandbox; the file name is the sandbox name. Sandbox names
use letters, digits, `_`, and `-` only; a name never contains `.`, so every
`<name>.<type>` directory unambiguously belongs to `<name>`. The name
`sandbox` is reserved for unnamed sandboxes. Unknown keys are
errors. `run`, `task`, `mount`, `umount`, `status`, and `stop` only accept
configured sandboxes; `psbox sandbox` runs unnamed ones (see
[cli.md](cli.md)). [examples/](../examples/) has configs for several
applications.

## Keys

| Key | Default | Meaning |
| --- | --- | --- |
| `home` | `private` | `private` (`<root>/<name>`), `tmpfs`, `none`, or a host path |
| `cache` | `tmpfs` | `tmpfs` hides `~/.cache`; `home` keeps it |
| `devices` | `["gpu", "audio"]` | [Device classes](#devices) |
| `env` | | Extra environment for every process |
| `rw` / `ro` | | Host paths shared read-write or read-only at the same path |
| `[mount.<name>]` | | `host`, `at`, `mode`, `optional` (see [Mounts](#mounts)) |
| `[command.<name>]` | the sandbox name | `exec`, `env`, `app-id`, `desktop` |
| `[autostart.<id>]` | | `command` or `hidden = true`, plus `from`, `entry`, `set`, `remove-args` |
| `[task.<name>]` | | A oneshot task (see [Tasks](#instances-and-tasks)) |
| `extends` | | [Profiles](#profiles) merged under this config |
| `dbus` | `portal` | `portal`: a private session bus with the host gateway; `host`: the host session and system buses, unfiltered; `off`: no session bus |
| `dbus-talk` | `[]` | Host session bus names the sandbox may call through the gateway (see [Host bus names](#host-bus-names)) |
| `[open-urls]` | `host` for each | `http`, `https`, `mailto`: `host` opens those links in the host's default app, `sandbox` inside the sandbox (see [Links](#links)) |
| `display` | `true` | Wayland (and Xwayland when `xwayland` is on) |
| `xwayland` | `off` | A private Xwayland for X11 apps: `builtin` or `xwayland-satellite` (see [Display and Xwayland](#display-and-xwayland)) |
| `xwayland-scale` | `native` | How X11 apps meet a scaled display, with `xwayland = "builtin"`: `native`, `round-up`, or `compositor` (see [X11 apps on a scaled display](#x11-apps-on-a-scaled-display)) |
| `network` | `isolated` | `isolated`, `host`, or `none` (see [Network](#network)) |
| `app-id` | | App id registered with the host portal (a desktop file id) |
| `idle-timeout` | `60` | Seconds without workload before the sandbox stops |

A command's `app-id` overrides the sandbox's for processes that command
starts, including their children. A sandbox without commands has one named
after the sandbox that runs the program of that name. `psbox run` without
`--command` uses the command named like the sandbox, or the only one.

```toml
home = "private"
rw = ["~/Projects/"]

[mount.ssh]
host = "~/.ssh/"
mode = "ro"
optional = true

[command.code]
exec = ["code"]
desktop = { id = "code-oss" }

[command.code-url-handler]
exec = ["code", "--open-url"]
desktop = { id = "code-oss-url-handler" }
```

## Paths and tokens

Host paths and mount destinations may start with a token:

| Token | Expands to |
| --- | --- |
| `~`, `~/...` | The host home (the same path inside the sandbox) |
| `@root`, `@root/...` | The sandbox root, such as `@root/steam` |
| `@self`, `@self/...` | `<root>/<name>` |
| `@self.<type>`, `@self.<type>/...` | `<root>/<name>.<type>`, such as `@self.bin` |

Other paths must be absolute and must not contain `..`.

A host path in `rw`, `ro`, or a mount's `host` that ends with `/` is a
directory. When it is missing and its mount is not `optional`, psbox creates
it: always inside the sandbox root (the supervisor does, as it starts or
when a live config change adds the share), and elsewhere only when `run`,
`task`, or `sandbox` is given `--create-dirs`. A `/` path that exists but is
not a directory is an error. Paths without the `/` are never created.

A host path may be a symlink: the sandbox gets what it points to. A symlink
to a path that does not exist is reported by the startup check, and nothing
is created through it. Inside places a sandbox can write to (its entry in
the sandbox root, such as `~/Sandbox/firefox`, and its read-write shares),
psbox does not follow symlinks in a host path, since an app could have
planted one to point a mount elsewhere; the startup check reports such a
link. The entry or share itself may be a symlink, for example to keep a
sandbox's files on another disk.

## Mounts

`rw` and `ro` share host paths at the same path. `[mount.<name>]` tables
mount a host path elsewhere, or mask a path:

| `mode` | What the sandbox gets at `at` |
| --- | --- |
| `ro` | The host path, read-only |
| `rw` | The host path, read-write |
| `overlay` | The host path with writes going to a private layer that is discarded with the sandbox |
| `empty` | An empty tmpfs, hiding whatever is there (no `host`) |

`at` defaults to `host`. With `optional = true`, a missing host path skips
the mount instead of stopping the sandbox.

Mounts are applied parents first, after everything psbox mounts itself. A
destination that does not exist is created inside whatever is mounted above
it: under a read-write share of a directory you can write, the directory
appears on the host; under a read-only mount (such as `/usr` or `/etc`), the
sandbox does not start, and the [startup check](cli.md#the-startup-check)
names the host path to create. Paths containing `:` cannot be bound.

A share or mount of `$XDG_RUNTIME_DIR` or anything that contains part of it
hands the sandbox your account, like mounting the Docker socket into a
container: `$XDG_RUNTIME_DIR/psbox` holds the supervisor sockets, through
which a process can mount any host path into a sandbox or start commands in
other sandboxes, and `$XDG_RUNTIME_DIR/bus` is the host session bus, which
can start any program on the host.

## Environment

Every process gets `HOME`, `USER`, `LOGNAME`, `XDG_RUNTIME_DIR`, a `PATH` of
`/usr/local/bin:/usr/bin`, the sandbox's `env`, its command's `env`, and
only these variables from the client that launched it: `TERM`, `COLORTERM`,
`LANG`, `LANGUAGE`, `LC_*`, `TZ`, `NO_COLOR`, `XDG_CURRENT_DESKTOP`,
`XDG_SESSION_DESKTOP`, `DESKTOP_SESSION`, `XDG_ACTIVATION_TOKEN`,
`DESKTOP_STARTUP_ID`.

With a display, `WAYLAND_DISPLAY` and `XDG_SESSION_TYPE=wayland` are set,
and `DISPLAY` when the sandbox has Xwayland. With `dbus = "portal"`, processes also get
`DBUS_SESSION_BUS_ADDRESS` for the sandbox bus and `GIO_USE_PORTALS=1`,
`GTK_USE_PORTAL=1`, `GDK_DEBUG=portals`, and
`GNOTIFICATION_BACKEND=portal`, so GLib, GTK 3, GTK 4, and GNotification use
portals. With a link handler, `XDG_CONFIG_DIRS` and `XDG_DATA_DIRS` put it
first (see [Links](#links)).

## Devices

`devices` lists device classes. Omitted, it is `["gpu", "audio"]`; a listed
value replaces that default.

| Class | Matches |
| --- | --- |
| `gpu` | `drm` render nodes (`dri/renderD*`) |
| `camera` | `video4linux` devices with `:capture:` in `ID_V4L_CAPABILITIES` |
| `gamepad` | `input` nodes with `ID_INPUT_JOYSTICK=1`, excluding keyboards and devices tagged `power-switch`; and `hidraw` nodes whose HID device has such an input node |
| `security-key` | `hidraw` nodes with `ID_SECURITY_TOKEN=1` (set by systemd's `60-fido-id.rules` from the HID report descriptor): the FIDO interface of a key, for passkeys and U2F, without its other interfaces such as an OTP keyboard |
| `alsa` | `sound` nodes under `snd/`, for apps that bypass PipeWire |
| `usb:<vid>:<pid>` | Every node whose `ID_VENDOR_ID` and `ID_MODEL_ID` match (four hex digits each) |
| `audio` | Not a `/dev` class: the host `pipewire-0` and `pulse/native` sockets |

Devices follow hotplug: a matching device that appears while the sandbox
runs is published into its `/dev`, and removed when it goes away. `psbox
devices` lists the devices the classes match; `psbox devices --all` lists
every device with the IDs `usb:` needs.

The `gamepad` exclusions exist because udev marks some keyboard interfaces
as joysticks (such as a keyboard's "System Control" interface) and grants
them `uaccess`. `audio` passes
the host sockets through unfiltered: playback, capture, PipeWire camera
nodes, and loading `pipewire-pulse` modules (see
[isolation.md](isolation.md)).

## Network

| `network` | What the sandbox gets |
| --- | --- |
| `isolated` | Its own network namespace with outbound connections through `pasta`: no port forwarding in either direction, no route to the host's loopback, DNS relayed to the host's resolver |
| `host` | The host network namespace (host abstract sockets stay unreachable) |
| `none` | Its own network namespace with only loopback |

A sandbox that does not set `network` gets `isolated` when `pasta` is on
`PATH`, and otherwise the host network, with a warning from `run`,
`sandbox`, and `task` (and in the sandbox's log): install pasta, or set
`network = "host"`. A sandbox that sets `network = "isolated"` does not
start without pasta. `list` and `status` show the mode in use, `host (no
pasta)` for the fallback.

Use `host` for apps that need the local network or incoming connections:
LAN discovery (casting, Steam Remote Play), hosting, and logins
that redirect to an app listening on `localhost`. Changing `network` needs a
restart, and so does installing pasta for a sandbox on the fallback.

## Display and Xwayland

With `display = true` (the default) and a Wayland session, the host's
Wayland socket is forwarded. Apps that support Wayland need nothing more;
with no `DISPLAY`, GTK, Qt, Electron, and SDL choose Wayland themselves.

X11 apps need `xwayland`: the sandbox then gets a private Xwayland on its
own display number, never the host's X server. `xwayland-satellite`,
started on the first X client, runs it and turns each X11 window into a
Wayland window on the host (see [architecture.md](architecture.md#xwayland)):

| `xwayland` | X11 window title bars |
| --- | --- |
| `off` (default) | No X server |
| `builtin` | Drawn by psbox: an Adwaita title bar with minimize, maximize, and close buttons, and resize borders |
| `xwayland-satellite` | Satellite's own. On GNOME that is a title and a close button, with no resize borders |

Both need `xwayland-satellite` and `Xwayland`; a sandbox without them does
not start, and the startup check names what is missing.

psbox draws title bars only where the compositor draws none for
satellite, as on GNOME; elsewhere the compositor draws them, as with
`xwayland-satellite`. Under `builtin`, satellite sees only the compositor
globals it needs for X11 windows, so it does not pass the old `wl_drm`
interface on to Xwayland (current Mesa uses `zwp_linux_dmabuf_v1`).
They follow the X11 window's wish: a window that draws its own decorations
gets none. The title bar's light or dark style comes from the settings
portal when satellite starts, its font from GNOME's `titlebar-font`
setting (as the sandbox sees it).

### X11 apps on a scaled display

X11 apps draw in X pixels, and satellite decides how many screen pixels
one is. `xwayland-scale` chooses, for `xwayland = "builtin"` (psbox's
title bar proxy changes the scale satellite sees, so other `xwayland`
values take only the default):

| `xwayland-scale` | Apps draw at | On a 150% display |
| --- | --- | --- |
| `native` (default) | The display's real pixels, told its scale through `Xft.dpi` and, rounded down, GTK's window scale | Sharp. Apps that scale by `Xft.dpi` are the right size; apps that use only the whole-number scale (REAPER's toolkit) draw at 100% and look small, and GTK scales its text but not its graphics; apps that round 1.5 up themselves (Qt 5) look large |
| `round-up` | The next whole scale (2 at 150%), which the compositor shrinks to the display's | Apps that scale themselves are the right size and nearly sharp, at the cost of drawing more pixels; apps that do not scale at all look smaller than with `native` |
| `compositor` | 1, which the compositor enlarges | Every app is the right size, and blurry |

At a whole-number scale, `round-up` is the same as `native`. One X server
serves every display, so on displays at different scales X11 apps get one
scale for all of them (the smallest, as satellite chooses it).

## Host bus names

With `dbus = "portal"`, the sandbox reaches only the portals and
notifications on the host bus. `dbus-talk` lists further host session bus
names it may call, each an explicit opt-in to that host service:

```toml
dbus-talk = ["org.freedesktop.secrets"]
```

Nothing is listed by default. Calls to a listed name, and their replies,
pass through the gateway unfiltered, with the caller's own host connection;
the service sees psbox as an ordinary host app. Signals from these services
are not forwarded, so services that report results only through signals
work only in part. Names are well-known bus names (at least two
dot-separated elements); `org.freedesktop.DBus` and the names the gateway
already serves (`org.freedesktop.portal.Desktop` and
`org.freedesktop.Notifications`) cannot be listed, and the key is an error
with `dbus = "host"` (which reaches every name) or `"off"`. While the
sandbox runs, the gateway owns every listed name on its bus, so nothing
inside can take it or have it activated, and host service files for listed
names are left out of the bus's activatable services. A task with
`dbus = "portal"` gets the sandbox's list.

## Links

In a sandbox with a session bus, `http`, `https`, and `mailto` links an app
opens go to the host's default app through the OpenURI portal. When that
app is itself a sandbox installed with `psbox install` (a browser or a mail
client, say), the link opens in that sandbox. Without this, GLib and
`xdg-open` would start the host's browser or mail client inside the
sandbox, from its desktop entry in `/usr`.

`[open-urls]` keeps a scheme's links inside the sandbox instead, for the
app that handles them itself:

```toml
[open-urls]
mailto = "sandbox"   # a mail client; http and https still go to the host
```

Each of `http`, `https`, and `mailto` is `host` (the default) or `sandbox`;
profiles merge the table key by key, and the built-in `browser` profile
keeps `http` and `https`. An app whose own links went to the host would
also report that it is not the default app for them. The table chooses the
sandbox's default handler for each scheme; it does not restrict the portal.
An app that calls `OpenURI` itself, as GTK 4 apps do here, still opens the
link with the host's default app, which for a sandboxed default app is that
sandbox.

The sandbox gets a link handler of its own in `/run/psbox`: a
`mimeapps.list` naming it the default for the schemes that go to the host,
its desktop entry, which runs `psbox open-uri` (psbox itself, read-only),
and `XDG_CONFIG_DIRS` and `XDG_DATA_DIRS` that find them before the
host's. The activation token of the app that opened the link goes along, so
the window that shows the link can come to the front. GLib and GTK apps use the
handler, as does `xdg-open` on GNOME and outside a desktop session, which
Electron and Chromium apps run. On KDE and Xfce, `xdg-open` goes by those
desktops' own browser settings before `mimeapps.list`. A default the
sandbox's own `~/.config/mimeapps.list` sets takes precedence. So do
`XDG_CONFIG_DIRS` and `XDG_DATA_DIRS` in `env`; to add a directory, keep
`/run/psbox/config` or `/run/psbox/share` first. With every scheme kept
inside, there is no handler.

## Profiles

`extends = ["name", ...]` merges profiles from
`$XDG_CONFIG_HOME/psbox/profiles/<name>.toml` (the same format; profiles may
extend others) under the config, in order. Tables merge key by key; `rw`,
`ro`, `devices`, and `dbus-talk` accumulate; other values from the config
replace the profile's.

The built-in `browser` profile adds the `camera` and `security-key` classes
and a shared `~/Downloads/`, and keeps `http` and `https` links in the
browser (`[open-urls]` `http` and `https` set to `sandbox`). A user profile
named `browser` replaces it.

## Instances and tasks

`psbox run <sandbox> -i <name>` starts or joins a named instance: its own
namespaces and processes, sharing the sandbox's home.

`[task.<name>]` defines a oneshot task, run with `psbox task <sandbox>
<task>` in its own supervisor; the command waits for it and returns its exit
code. A task sees the sandbox's mounts, with the mounts named in `writable`
read-write. It has no display, no session bus, and no devices unless it sets
`dbus` or `devices`. `script` runs with `bash -c`.

An overlay does not reliably show changes made to its host directory while
it is mounted, so when a task ends, `psbox task` has each running instance
of the sandbox replace its overlays over the task's `writable` directories
with fresh ones. The swap is atomic: a program already running from the old
overlay keeps its files until it exits, the next one sees the task's
changes, and whatever was written to the old overlay is discarded. Mounts
beneath the overlay, including live ones, are attached again on top. An
overlay is not refreshed when a tmpfs or another overlay is mounted beneath
it (such as one at `/tmp` in a sandbox with Xwayland), whose contents would
be lost, or when something else is mounted over it, at its path or a
parent; that instance warns and keeps the old files until it restarts.

| Task key | Meaning |
| --- | --- |
| `exec` or `script` | The command line, or a `bash -c` script (exactly one) |
| `home`, `cache` | Override the sandbox's |
| `writable` | Names of `[mount.<name>]` entries mounted read-write |
| `devices` | Device classes; none unless listed |
| `dbus` | Session bus; `off` unless set |
| `network` | Overrides the sandbox's (inherited otherwise, including whether it was set at all) |
| `env` | Extra environment |

```toml
[mount.bin]
host = "@self.bin/"
at = "~/.bin"
mode = "overlay"

[task.update]
home = "tmpfs"
writable = ["bin"]
script = '''
set -euo pipefail
cd ~/.bin
...
'''
```

## Desktop and autostart entries

`psbox install` writes `$XDG_DATA_HOME/applications/<id>.desktop` for every
command with a `desktop` table, and `$XDG_CONFIG_HOME/autostart/<id>.desktop`
for every `autostart` table, in every sandbox config:

```toml
[command.code]
exec = ["code"]
desktop = { id = "code-oss", remove-args = ["--new-window"], set = { StartupWMClass = "code-oss" } }

[autostart.steam-autostart]
hidden = true              # hides the host's autostart entry with this id
```

| Key | Meaning |
| --- | --- |
| `id` | Desktop file id; defaults to the command's `app-id`, else the command name. Also the command's default `app-id` |
| `from` | Host entry to copy (from `$XDG_DATA_DIRS/applications`, or `$XDG_CONFIG_DIRS/autostart` first for autostart); defaults to `id` |
| `entry` | The whole entry inline, instead of copying |
| `set` | Keys to set in `[Desktop Entry]` |
| `remove-args` | Arguments dropped from every `Exec` line (field codes stay) |

Every `Exec` (including desktop actions) becomes `psbox run <sandbox>
--command <command> -- <args>`: the program is dropped and its arguments
kept. An `Exec` that names a path or starts with `env` runs exactly as
written, with `--exec`. `TryExec` is removed, `DBusActivatable=false` is set
(activation would start the app on the host), and `Icon` values starting
with `~` or `@` are expanded.

Files start with `# @generator psbox` and a `# @sandbox <name>` line naming
the config they came from. `install` replaces such files and removes those
no longer configured (including psbox files without an `@sandbox` line), and
never touches other files. A sandbox whose config fails to load keeps its
files, and so does an entry that cannot be built.

GNOME matches X11 windows by their X class, which must appear as
`StartupWMClass` in the entry for the window to group under it.

## Live changes

A running sandbox (not a task) watches its config file and the profiles
directory. Binds and tmpfs mounts (`rw`, `ro`, and `ro`, `rw`, or `empty`
mounts, and the audio sockets) are attached and detached live, and device
classes are re-published, along with the portals they allow (Camera with
`camera`, Realtime with `audio`); environment, commands, app ids, and the
idle timeout apply to the next process. Mounts beneath a changed mount, including
live mounts made by `psbox mount` or the file chooser, are detached and
attached again with it. New mounts are prepared before
anything is detached, so a missing source leaves the sandbox unchanged; a
mount that still fails to apply marks the sandbox for a restart.

Changes to `home`, `cache`, `dbus`, `dbus-talk`, `open-urls`, `xwayland`,
`xwayland-scale`, `network`, or overlay mounts need a restart: nothing is applied live, the sandbox stops as soon
as it is idle, and until then `psbox run` warns and `psbox status` shows the
pending restart. A change that fails the startup check is not applied.
