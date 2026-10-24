# Configuration

## Locations

| What | Path |
| --- | --- |
| Sandbox configs | `$XDG_CONFIG_HOME/psbox/sandboxes/<name>.jsonnet` |
| Profiles (by convention) | `$XDG_CONFIG_HOME/psbox/profiles/<name>.libsonnet` |
| Sandbox root | `$PSBOX_ROOT`, default `~/Sandbox` |
| A sandbox's storage, its default home | `<root>/<name>` |
| Other sandbox directories | `<root>/<name>.<type>`, such as `~/Sandbox/retroarch.bin` |
| Supervisor sockets | `$XDG_RUNTIME_DIR/psbox/<key>.sock` |

One [Jsonnet](https://jsonnet.org/) file per sandbox; the file name is the
sandbox name. Sandbox names use letters, digits, `_`, and `-` only; a name
never contains `.`, so every `<name>.<type>` directory unambiguously belongs
to `<name>`. The name `sandbox` is reserved for unnamed sandboxes. `run`,
`task`, `config`, `mount`, `umount`, `status`, and `stop` only accept
configured sandboxes; `psbox sandbox` runs unnamed ones (see
[cli.md](cli.md)). [examples/](../examples/) has configs for several
applications.

## How a config is read

A config is a Jsonnet object. psbox evaluates it on top of a base that has
the sections that are on unless a config removes them, then checks the
result and fills in every default:

```jsonnet
local psbox = import 'psbox';

{
  home+: { cache: 'persistent' },
  dbus+: { talk+: ['org.freedesktop.secrets'] },
  mount+: {
    projects: psbox.mount.readWrite(psbox.path([psbox.home, 'Projects']), create=true),
  },
}
```

- Merging is Jsonnet's own. A field written `section+: {...}` merges into
  what is already there (the base, a profile); `section: {...}` replaces
  it. The same goes for lists: `talk+: [...]` appends, `talk: [...]`
  replaces. Forgetting the `+` silently drops what a profile set, so
  `psbox config <sandbox>` prints the config as psbox sees it.
- `null` removes: a section (`dbus: null` is no session bus), or a named
  entry (`mount+: { downloads: null }`). It is accepted only where "not
  there" means something: `home`, `dbus`, `network`, `display`,
  `xwayland`, and the entries of `mount`, `app`, `task`, and
  `environment`. Turning a removed section back on means writing it out
  again, for example `dbus: psbox.defaults.dbus + { mode: 'host' }`.
  Jsonnet cannot merge into a `null`: after a profile's `dbus: null`, write
  `dbus: { ... }`, not `dbus+: { ... }`.
- A present section is on, with its defaults filled in for the keys it
  leaves out. Keys that only apply to another mode are errors (such as
  `path` with a temporary home), as are unknown keys and wrong types.
  Hidden fields (`::`) never reach psbox, so a config that hides one is an
  error rather than one that silently loses it; helpers belong in `local`.
- Every problem is reported at once, by the key path it is at, such as
  `mount.bin.mode`. Jsonnet errors name the file, line, and column.

## The psbox library

`import 'psbox'` returns psbox's library, made for the sandbox being
evaluated:

| Field | What it is |
| --- | --- |
| `psbox.name` | The sandbox name (`null` in an unnamed sandbox) |
| `psbox.home` | The host home, which is also the home's path inside the sandbox |
| `psbox.root` | The sandbox root |
| `psbox.storage` | `<root>/<name>` (an error in an unnamed sandbox, when used) |
| `psbox.storageFor(type)` | `<root>/<name>.<type>`, such as `psbox.storageFor('bin')`; `type` is letters, digits, `_`, and `-` |
| `psbox.path([segments])` | Joins segments with `/` (see [Paths](#paths)) |
| `psbox.mount.readWrite(source, destination=, optional=, create=)` | A complete read-write mount; also `readOnly` and `overlay`, and `empty(destination)` |
| `psbox.devices.none` | Every device class off |
| `psbox.defaults.<section>` | A section's full default |
| `psbox.profiles.<name>` | A built-in profile |

Plain objects are equally valid: `psbox.mount.readOnly('/srv/games')` is
`{ source: '/srv/games', mode: 'read-only', optional: false, create: false }`.

## Sections

| Section | Default | Meaning |
| --- | --- | --- |
| `sandbox` | `{ 'idle-timeout': 60, '32-bit': false }` | `idle-timeout`: seconds without workload before the sandbox stops. `'32-bit': true` lets 32-bit x86 programs run, at the cost of more kernel exposure (see [seccomp](isolation.md#namespaces-and-processes)) |
| `home` | `{ mode: 'path', path: psbox.storage, cache: 'temporary' }` | `mode: 'path'` binds `path` read-write as the home; `'temporary'` is an empty tmpfs; `null` is no home. `cache: 'temporary'` hides `~/.cache`, `'persistent'` keeps it (path mode only) |
| `devices` | `gpu` and `audio` on | [Device classes](#devices), each `true` or `false`, and `usb` |
| `dbus` | `{ mode: 'portal', talk: [] }` | `portal`: a private session bus with the host gateway; `host`: the host session and system buses, unfiltered; `null`: no session bus. `talk`: [host bus names](#host-bus-names) (portal mode) |
| `network` | `{ mode: 'automatic' }` | `automatic`, `isolated`, or `host`; `null` is loopback only (see [Network](#network)) |
| `display` | `{}` | Wayland (and Xwayland when `xwayland` is set); `null` is no display |
| `xwayland` | absent | A private Xwayland for X11 apps: `mode` `builtin` (the default) or `xwayland-satellite`, and `scale` (see [Display and Xwayland](#display-and-xwayland)) |
| `links` | `host` for each | `http`, `https`, `mailto`: `host` opens those links in the host's default app, `sandbox` inside the sandbox (see [Links](#links)) |
| `environment` | | Extra environment for every process; a `null` value removes a variable a profile set |
| `mount.<name>` | | `source`, `destination`, `mode`, `optional`, `create` (see [Mounts](#mounts)) |
| `app.<name>` | the default app | Applications and autostart entries (see [Apps](#apps)) |
| `task.<name>` | | A oneshot task (see [Tasks](#instances-and-tasks)) |

`display: null` also removes `xwayland`.

## Paths

Paths in a config are absolute and contain no `..`. `psbox.path` builds
them: it joins its segments with `/`, collapses repeated slashes, skips
empty and `.` segments, drops a trailing slash (except for `/` itself), and
treats a leading `/` on a later segment as a separator only. `..` is an
error:

```jsonnet
psbox.path([psbox.home, '.bin', 'AppRun'])   // /home/u/.bin/AppRun
psbox.path(['/foo/', '/.bin/'])              // /foo/.bin
```

`~` means nothing in a config. A `script` is run by bash, which expands it
itself.

A mount with `create: true` names a directory: when it is missing and the
mount is not `optional`, psbox creates it, always inside the sandbox root
(the supervisor does, as it starts or when a live config change adds the
mount), and elsewhere only when `run`, `task`, or `sandbox` is given
`--create-dirs`. A source that exists but is not a directory is then an
error. Sources without `create` are never created.

A host path may be a symlink: the sandbox gets what it points to. A symlink
to a path that does not exist is reported by the startup check, and nothing
is created through it. Inside places a sandbox can write to (its entry in
the sandbox root, such as `~/Sandbox/firefox`, and its read-write shares),
psbox does not follow symlinks in a host path, since an app could have
planted one to point a mount elsewhere; the startup check reports such a
link. The entry or share itself may be a symlink, for example to keep a
sandbox's files on another disk.

## Mounts

`mount.<name>` mounts a host path into the sandbox, or masks a path:

| `mode` | What the sandbox gets at `destination` |
| --- | --- |
| `read-write` (default) | The host path, read-write |
| `read-only` | The host path, read-only |
| `overlay` | The host path with writes going to a private layer that is discarded with the sandbox |
| `empty` | An empty tmpfs, hiding whatever is there (no `source`) |

`destination` defaults to `source`. With `optional: true`, a missing
source skips the mount instead of stopping the sandbox; `create: true`
cannot be combined with it, nor with `empty`. Two mounts at the same
destination are an error, unless they are the same mount (such as the
browser profile's `downloads` and your own mount of `~/Downloads`), which
merge. Mount names use letters, digits, `.`, `_`, and `-`.

Mounts are applied parents first, after everything psbox mounts itself. A
destination that does not exist is created inside whatever is mounted above
it: under a read-write share of a directory you can write, the directory
appears on the host; under a read-only mount (such as `/usr` or `/etc`), the
sandbox does not start, and the [startup check](cli.md#the-startup-check)
names the host path to create.

A share or mount of `$XDG_RUNTIME_DIR` or anything that contains part of it
hands the sandbox your account, like mounting the Docker socket into a
container: `$XDG_RUNTIME_DIR/psbox` holds the supervisor sockets, through
which a process can mount any host path into a sandbox or start commands in
other sandboxes, and `$XDG_RUNTIME_DIR/bus` is the host session bus, which
can start any program on the host.

## Environment

Every process gets `HOME`, `USER`, `LOGNAME`, `XDG_RUNTIME_DIR`, a `PATH` of
`/usr/local/bin:/usr/bin`, the sandbox's `environment`, its app's
`environment`, and only these variables from the client that launched it:
`TERM`, `COLORTERM`, `LANG`, `LANGUAGE`, `LC_*`, `TZ`, `NO_COLOR`,
`XDG_CURRENT_DESKTOP`, `XDG_SESSION_DESKTOP`, `DESKTOP_SESSION`,
`XDG_ACTIVATION_TOKEN`, `DESKTOP_STARTUP_ID`.

With a display, `WAYLAND_DISPLAY` and `XDG_SESSION_TYPE=wayland` are set,
and `DISPLAY` when the sandbox has Xwayland. With the private bus
(`dbus.mode: 'portal'`), processes also get `DBUS_SESSION_BUS_ADDRESS` for
the sandbox bus, and a launch with a [portal identity](#portal-identity)
gets `GIO_USE_PORTALS=1`, `GTK_USE_PORTAL=1`, `GDK_DEBUG=portals`, and
`GNOTIFICATION_BACKEND=portal`, so GLib, GTK 3, GTK 4, and GNotification use
portals. A launch without one gets none of these, even when the config sets
them, so toolkits use their own dialogs. With a link handler,
`XDG_CONFIG_DIRS` and `XDG_DATA_DIRS` put it first (see [Links](#links)).

## Devices

`devices` turns device classes on and off; by default `gpu` and `audio` are
on. Writing `devices: { camera: true }` keeps the other defaults;
`psbox.devices.none + { camera: true }` is the camera alone.

| Key | Matches |
| --- | --- |
| `gpu` | `drm` render nodes (`dri/renderD*`) |
| `camera` | `video4linux` devices with `:capture:` in `ID_V4L_CAPABILITIES` |
| `gamepad` | `input` nodes with `ID_INPUT_JOYSTICK=1`, excluding keyboards and devices tagged `power-switch`; and `hidraw` nodes whose HID device has such an input node |
| `security-key` | `hidraw` nodes with `ID_SECURITY_TOKEN=1` (set by systemd's `60-fido-id.rules` from the HID report descriptor): the FIDO interface of a key, for passkeys and U2F, without its other interfaces such as an OTP keyboard |
| `alsa` | `sound` nodes under `snd/`, for apps that bypass PipeWire |
| `usb` | A list of `<vendor>:<product>` ids (four hex digits each): every node whose `ID_VENDOR_ID` and `ID_MODEL_ID` match |
| `audio` | Not a `/dev` class: the host `pipewire-0` and `pulse/native` sockets |

Devices follow hotplug: a matching device that appears while the sandbox
runs is published into its `/dev`, and removed when it goes away. `psbox
devices` lists the devices the classes match; `psbox devices --all` lists
every device with the IDs `usb` needs.

The `gamepad` exclusions exist because udev marks some keyboard interfaces
as joysticks (such as a keyboard's "System Control" interface) and grants
them `uaccess`. `audio` passes the host sockets through unfiltered:
playback, capture, PipeWire camera nodes, and loading `pipewire-pulse`
modules (see [isolation.md](isolation.md)).

## Network

| `network` | What the sandbox gets |
| --- | --- |
| `{ mode: 'automatic' }` (default) | `isolated` when `pasta` is on `PATH`, otherwise the host network, with a warning |
| `{ mode: 'isolated' }` | Its own network namespace with outbound connections through `pasta`: no port forwarding in either direction, no route to the host's loopback, DNS relayed to the host's resolver. Without pasta the sandbox does not start |
| `{ mode: 'host' }` | The host network namespace (host abstract sockets stay unreachable) |
| `null` | Its own network namespace with only loopback |

The automatic fallback warns from `run`, `sandbox`, and `task` (and in the
sandbox's log): install pasta, or set `network: { mode: 'host' }`. `list`
and `status` show the mode in use, `host (no pasta)` for the fallback.

Use `host` for apps that need the local network or incoming connections:
LAN discovery (casting, Steam Remote Play), hosting, and logins that
redirect to an app listening on `localhost`. Changing `network` needs a
restart, and so does installing pasta for a sandbox on the fallback.

## Display and Xwayland

With a display (the default) and a Wayland session, the host's Wayland
socket is forwarded. Apps that support Wayland need nothing more; with no
`DISPLAY`, GTK, Qt, Electron, and SDL choose Wayland themselves.

X11 apps need `xwayland`: the sandbox then gets a private Xwayland on its
own display number, never the host's X server. `xwayland-satellite`,
started on the first X client, runs it and turns each X11 window into a
Wayland window on the host (see [architecture.md](architecture.md#xwayland)):

| `xwayland.mode` | X11 window title bars |
| --- | --- |
| `builtin` (default) | Drawn by psbox: an Adwaita title bar with minimize, maximize, and close buttons, and resize borders |
| `xwayland-satellite` | Satellite's own. On GNOME that is a title and a close button, with no resize borders |

`xwayland: {}` turns Xwayland on with the defaults. Both modes need
`xwayland-satellite` and `Xwayland`; a sandbox without them does not start,
and the startup check names what is missing.

psbox draws title bars only where the compositor draws none for
satellite, as on GNOME; elsewhere the compositor draws them, as in the
`xwayland-satellite` mode. In the builtin mode, satellite sees only the
compositor globals it needs for X11 windows, so it does not pass the old
`wl_drm` interface on to Xwayland (current Mesa uses `zwp_linux_dmabuf_v1`).
Title bars follow the X11 window's wish: a window that draws its own
decorations gets none. The title bar's light or dark style comes from the
settings portal when satellite starts, its font from GNOME's
`titlebar-font` setting (as the sandbox sees it).

### X11 apps on a scaled display

X11 apps draw in X pixels, and satellite decides how many screen pixels
one is. `xwayland.scale` chooses, in the builtin mode (psbox's title bar
proxy changes the scale satellite sees, so the `xwayland-satellite` mode
takes no `scale`):

| `xwayland.scale` | Apps draw at | On a 150% display |
| --- | --- | --- |
| `native` (default) | The display's real pixels, told its scale through `Xft.dpi` and, rounded down, GTK's window scale | Sharp. Apps that scale by `Xft.dpi` are the right size; apps that use only the whole-number scale (REAPER's toolkit) draw at 100% and look small, and GTK scales its text but not its graphics; apps that round 1.5 up themselves (Qt 5) look large |
| `round-up` | The next whole scale (2 at 150%), which the compositor shrinks to the display's | Apps that scale themselves are the right size and nearly sharp, at the cost of drawing more pixels; apps that do not scale at all look smaller than with `native` |
| `compositor` | 1, which the compositor enlarges | Every app is the right size, and blurry |

At a whole-number scale, `round-up` is the same as `native`. One X server
serves every display, so on displays at different scales X11 apps get one
scale for all of them (the smallest, as satellite chooses it).

## Host bus names

With the private bus, the sandbox reaches only the portals and
notifications on the host bus. `dbus.talk` lists further host session bus
names it may call, each an explicit opt-in to that host service:

```jsonnet
{ dbus+: { talk+: ['org.freedesktop.secrets'] } }
```

Nothing is listed by default. Calls to a listed name, and their replies,
pass through the gateway unfiltered, with the caller's own host connection;
the service sees psbox as an ordinary host app. Signals from these services
are not forwarded, so services that report results only through signals
work only in part. Names are well-known bus names (at least two
dot-separated elements); `org.freedesktop.DBus` and the names the gateway
already serves (`org.freedesktop.portal.Desktop` and
`org.freedesktop.Notifications`) cannot be listed, and the key is an error
with `mode: 'host'`, which reaches every name. While the sandbox runs, the
gateway owns every listed name on its bus, so nothing inside can take it or
have it activated, and host service files for listed names are left out of
the bus's activatable services.

## Links

In a sandbox with a session bus, `http`, `https`, and `mailto` links an app
opens go to the host's default app through the OpenURI portal. When that
app is itself a sandbox installed with `psbox install` (a browser or a mail
client, say), the link opens in that sandbox. Without this, GLib and
`xdg-open` would start the host's browser or mail client inside the
sandbox, from its desktop entry in `/usr`.

`links` keeps a scheme's links inside the sandbox instead, for the app that
handles them itself:

```jsonnet
{ links+: { mailto: 'sandbox' } }   // a mail client; http and https still go to the host
```

Each of `http`, `https`, and `mailto` is `host` (the default) or `sandbox`;
the built-in `browser` profile keeps `http` and `https`. An app whose own
links went to the host would also report that it is not the default app for
them. `links` chooses the sandbox's default handler for each scheme; it
does not restrict the portal. An app that calls `OpenURI` itself, as GTK 4
apps do here, still opens the link with the host's default app, which for a
sandboxed default app is that sandbox.

The sandbox gets a link handler of its own in `/run/psbox`: a
`mimeapps.list` naming it the default for the schemes that go to the host,
its desktop entry, which runs `psbox open-uri` (psbox itself, read-only),
and `XDG_CONFIG_DIRS` and `XDG_DATA_DIRS` that find them before the
host's. The activation token of the app that opened the link goes along, so
the window that shows the link can come to the front. GLib and GTK apps use
the handler, as does `xdg-open` on GNOME and outside a desktop session,
which Electron and Chromium apps run. On KDE and Xfce, `xdg-open` goes by
those desktops' own browser settings before `mimeapps.list`. A default the
sandbox's own `~/.config/mimeapps.list` sets takes precedence. So do
`XDG_CONFIG_DIRS` and `XDG_DATA_DIRS` in `environment`; to add a directory,
keep `/run/psbox/config` or `/run/psbox/share` first. With every scheme
kept inside, there is no handler.

## Profiles

A profile is a Jsonnet file a config imports and adds itself to:

```jsonnet
// ~/.config/psbox/profiles/gaming.libsonnet
local psbox = import 'psbox';
{
  devices+: { gamepad: true },
  xwayland: {},
  mount+: { roms: psbox.mount.readOnly(psbox.path([psbox.home, 'Games', 'ROMs'])) },
}
```

```jsonnet
// ~/.config/psbox/sandboxes/retroarch.jsonnet
(import '../profiles/gaming.libsonnet') + { mount+: { roms: null } }
```

Imports resolve relative to the importing file. A profile written with
`+:` adds to what is beneath it; one written with `:` replaces it.

The built-in `browser` profile, `psbox.profiles.browser`, turns on the
`camera` and `security-key` classes, mounts `~/Downloads` read-write
(created when missing), and keeps `http` and `https` links in the browser.
`psbox sandbox --profile NAME` uses
`$XDG_CONFIG_HOME/psbox/profiles/NAME.libsonnet`, or the built-in profile of
that name.

### What a config may read

A sandbox must not be able to rewrite its own config, which psbox reloads
live. Every file evaluating a config reads (the config itself and every
`import`, `importstr`, and `importbin`) must be a regular file, and must
not be inside the sandbox root (`~/Sandbox`), nor anywhere the sandbox can
write: its home, its read-write mounts (directories and single files), its
tasks' read-write mounts, and the read-write mounts added to the running
sandbox by `psbox mount` or a file dialog. Overlay, read-only, and empty
mounts are fine. Each file is checked as it is opened and read through the
descriptor that was checked; evaluation fails when one is in such a place.
There is no way to turn the check off.

The check covers what this sandbox can write. It does not cover:

- another sandbox's read-write shares outside the sandbox root: a sandbox
  that can write the directory a profile is in can change that profile for
  every sandbox that imports it;
- a file written while a mount made it writable, then read after the
  restart that removed the mount;
- writes through a file a process opened before its mount was removed, or
  a mount added while the config was being evaluated;
- a hard link to the file in a writable place, or a symlink the sandbox
  replaces on the way to an import.

Keep profiles and configs where no sandbox can write.

## Apps

`app.<name>` entries are desktop entries, each one file, and for an
application, how to run it:

```jsonnet
local psbox = import 'psbox';
local bin = psbox.path([psbox.home, '.bin']);

{
  app+: {
    retroarch: {
      run: [psbox.path([bin, 'AppRun'])],
      id: 'com.libretro.RetroArch',
      'source-id': 'com.libretro.RetroArch',
      keys+: { Categories: ['Game', 'Emulator'], Name: { default: 'RetroArch', de: 'RetroArch (Sandbox)' } },
      actions+: { menu: { Name: 'Open Menu', arguments: ['--menu'] } },
    },
    'fix-playlists': { run: [psbox.path([bin, 'fix-playlists.py'])], 'no-display': true },
    'retroarch-autostart': { kind: 'autostart', app: 'retroarch', id: 'com.libretro.RetroArch' },
    'warpinator-autostart': { kind: 'autostart', suppress: true },
  },
}
```

| Key | Kind | Meaning |
| --- | --- | --- |
| `kind` | | `application` (default): `$XDG_DATA_HOME/applications/<id>.desktop`; `autostart`: `$XDG_CONFIG_HOME/autostart/<id>.desktop` |
| `id` | both | The desktop file id, default the entry's name. For an application, also its [portal identity](#portal-identity) |
| `run` | application | The command line (required, except for the default app) |
| `environment` | application | Extra environment for its processes |
| `app` | autostart | The application it starts; default the default app |
| `source-id` | both | Copy this host entry, one psbox did not write (from the `applications` directory of `$XDG_DATA_HOME`, then of `$XDG_DATA_DIRS`; for an autostart entry, the `autostart` directory of `$XDG_CONFIG_HOME`, then of `$XDG_CONFIG_DIRS`, first) |
| `keys` | both | Desktop Entry keys for `[Desktop Entry]` |
| `actions` | application | Actions by name: Desktop Entry keys and `arguments` |
| `remove-arguments` | both | Arguments dropped from every copied `Exec` line (field codes stay) |
| `no-display` | application | `true` writes `NoDisplay=true`: no menu entry, still an identity; `false` removes a copied one |
| `suppress` | both | `true` writes only `Hidden=true` under `id`, hiding the host's entry with that id; nothing else may be set |

`psbox run <sandbox> --app <name>` runs an application. The application
named after the sandbox is its default app, which `psbox run <sandbox>` runs.
Ids are unique per kind: an application and an autostart entry may share
one, and two of one kind may not.

### The default app

When a config does not mention `app.<sandbox name>`, the sandbox still has a
default app: it runs the program named like the sandbox, and when the host
has a desktop entry with that id (one psbox did not write), it inherits it,
as if it set `'source-id': '<name>'`. Without such an entry it gets no
desktop file, and so no portal identity; `psbox install` says so. A config
that defines the default app keeps what it writes and gets the rest filled
in: `run`, and the host entry, unless it sets `'source-id': null`.
`app+: { <name>: null }` removes it, and plain `psbox run <sandbox>` is then
an error.

### Desktop entries

An entry is a copy of the host's (`source-id`) or built from `keys`, with
`Name` defaulting to the entry's name.

- Key values are strings, booleans, lists of strings (written `a;b;`), or
  translations: `Name: { default: 'RetroArch', de: '...' }` writes `Name`
  and `Name[de]`. `null` removes a copied key and its translations.
  Setting a key drops the copied translations of it. Values are escaped as
  the Desktop Entry specification asks; other control characters are
  errors.
- `Exec`, `TryExec`, `DBusActivatable`, `Hidden`, `Type`, `Actions`, and
  `NoDisplay` cannot be set (psbox writes them), nor can a key with `[` or
  `]`.
- An action becomes `[Desktop Action <name>]` with an `Exec` that runs the
  app with its `arguments`; `null` removes a copied action. Actions merge
  by name onto the copied ones, and `Actions=` names the ones there are.
- Every copied `Exec` (including actions) becomes `psbox run <sandbox> --app
  <app> -- <args>`: the program is dropped and its arguments kept. One that
  names a path or starts with `env` runs exactly as written, with `--app
  <app> --exec`. An entry without an `Exec` gets one, with `%U` when it
  has a `MimeType`. `TryExec` is removed, `DBusActivatable=false` is set
  (activation would start the app on the host), copied `Hidden` and `Type`
  are dropped, and `Type=Application` is set.

`psbox install` writes the entries. Files start with `# @generator psbox`
and a `# @sandbox <name>` line naming the config they came from. `install`
replaces such files and removes those no longer configured (including psbox
files without an `@sandbox` line), and never touches other files. A sandbox
whose config fails to load keeps its files, and so does an entry that
cannot be built. When two sandboxes claim the same file, the sandbox whose
name sorts first gets it and `install` warns about the other. An entry with
the same id as a host one takes its place in menus and at login.

GNOME matches X11 windows by their X class, which must appear as
`StartupWMClass` in the entry for the window to group under it.

## Portal identity

Host portals (file dialogs, screen sharing, the camera, notifications,
printing, running in the background) remember what you allowed by app id,
and show the app's name and icon from its desktop entry. The host portal
reads an unsandboxed app's id from the systemd unit the desktop started it
in; psbox's processes all run in its own unit, so psbox registers each
sandboxed client with the host portal itself, under the identity of the
launch it belongs to.

The host portal accepts an id only when an application entry with that id
exists (`NoDisplay` entries count; `Hidden` ones do not), so when a launch
starts, psbox asks it, and the answer decides the launch's identity:

| Launch | Identity |
| --- | --- |
| An application | Its `id`, when the host portal accepts it and no other sandbox's installed entry claims it |
| An autostart entry | The application it starts |
| `psbox run <sandbox> --exec` | The default app's (with `--app`, that app's) |
| A task | None, unless the task sets `app: '<name>'` to use that application's |
| An unnamed sandbox | None, unless `psbox sandbox --app-id ID` is given (it must be accepted) |
| Services the private bus starts | The default app's (a task's own) |
| `xwayland-satellite` and the title bar proxy | None |

Only sandboxes with the private bus have identities; with `mode: 'host'`
processes reach the host portal directly, as unsandboxed ones. Each launch
runs in a cgroup of its own, so the processes it starts keep its identity
however they fork (see [architecture.md](architecture.md#portal-identity)).
A launch's identity is decided when it starts; config changes apply to
later launches. Identity is attribution, not a boundary: launches in one
sandbox share its home, and any of them can plant a session service that
runs with the default app's identity.

A process without an identity reaches only the portals that need none:
settings (the theme), opening links (under `links`), realtime priority,
inhibiting suspend, and the network, memory, and power monitors. Its
toolkit uses its own file dialog, which shows only the sandbox; screen
sharing, screenshots, the camera portal, printing, the notification
portal, global shortcuts, email, and the rest are unavailable (plain
notifications still reach the host). A new sandbox has no identity until
`psbox install` writes its entries, unless the host already has an entry
with the id; `psbox run` warns about an application that runs without one.
The host portal needs to be xdg-desktop-portal 1.19 or later.

## Instances and tasks

`psbox run <sandbox> -i <name>` starts or joins a named instance: its own
namespaces and processes, sharing the sandbox's home.

`task.<name>` defines a oneshot task, run with `psbox task <sandbox>
<task>` in its own supervisor; the command waits for it and returns its exit
code. A task runs with the sandbox's config, with its `overrides` added
the way a profile is (`+:` merges, `null` removes), so it gets the
sandbox's bus, display, and devices unless its overrides remove them. It
ignores `idle-timeout`. A task whose config does not validate makes the
sandbox's config invalid.

| Task key | Meaning |
| --- | --- |
| `run` or `script` | The command line, or a `bash -c` script (exactly one) |
| `app` | An application whose portal identity the task uses (needs a session bus) |
| `overrides` | Sections to change for the task; not `app` or `task` |

```jsonnet
local psbox = import 'psbox';
{
  mount+: { bin: psbox.mount.overlay(psbox.storageFor('bin'), destination=psbox.path([psbox.home, '.bin'])) },
  task+: {
    update: {
      script: |||
        set -euo pipefail
        cd ~/.bin
        ...
      |||,
      overrides: {
        home: { mode: 'temporary' },
        display: null,
        devices: psbox.devices.none,
        mount+: { bin+: { mode: 'read-write' } },
      },
    },
  },
}
```

An overlay does not reliably show changes made to its host directory while
it is mounted, so when a task ends, `psbox task` has each running instance
of the sandbox replace its overlays whose source overlaps (is, contains, or
is inside) a host path the task could write through a mount, symlinks
resolved, with fresh ones. The swap is atomic: a program already running
from the old overlay keeps its files until it exits, the next one sees the
task's changes, and whatever was written to the old overlay is discarded.
Mounts beneath the overlay, including live ones, are attached again on top.
An overlay is not refreshed when a tmpfs or another overlay is mounted
beneath it (such as one at `/tmp` in a sandbox with Xwayland), whose
contents would be lost, or when something else is mounted over it, at its
path or a parent; that instance warns and keeps the old files until it
restarts.

## Live changes

A running sandbox (not a task) watches the directories of every file its
config read, and evaluates the config again when anything in them changes;
a result that is the same as before changes nothing. For an import that
does not exist yet it watches the nearest existing parent, and after a
failed evaluation it also watches what that read, so fixing it reloads. It
does not watch directories above those: renaming or replacing a parent
directory of an imported file takes effect at the next reload or restart.
Directories that change often (an import directly in `~`, say) cost a
re-evaluation each time.

Binds and tmpfs mounts (`read-write`, `read-only`, and `empty` mounts, and
the audio sockets) are attached and detached live, and device classes are
re-published, along with the portals they allow (Camera with `camera`,
Realtime with `audio`); environment, apps, and the idle timeout apply to
the next process. Mounts beneath a changed mount, including live mounts
made by `psbox mount` or the file chooser, are detached and attached again
with it. New mounts are prepared before anything is detached, so a missing
source leaves the sandbox unchanged; a mount that still fails to apply
marks the sandbox for a restart.

Changes to `home`, `home.cache`, `dbus`, `dbus.talk`, `links`, `xwayland`,
`xwayland.scale`, `network`, `sandbox.32-bit`, or overlay mounts need a
restart: nothing is applied live, the sandbox stops as soon as it is idle,
and until then `psbox run` warns and `psbox status` shows the pending
restart. A change that fails the startup check is not applied.
