# Architecture

psbox is one binary. The same `psbox` is the client (`run`, `status`,
`install`, ...) and, as `psbox supervise <key>`, the supervisor of one
sandbox.

## Process model

```
psbox run <name>  --seqpacket + SCM_RIGHTS-->  psbox@<key>.service (socket-activated, Delegate=yes)
                                               supervisor: owns user + mount namespaces and the cgroup subtree
                                                 |- forker thread  (joined to the sandbox PID namespace)
                                                 |- infra/     init (PID 1), the private dbus-daemon and the services it activates,
                                                 |            the connector, xwayland-satellite and Xwayland, psbox decorations
                                                 '- workload/  apps, cloned straight into this cgroup
```

- The supervisor creates its user and mount namespaces as it starts, before
  any thread exists, maps only the caller's uid and gid, and restricts itself
  to a Landlock domain that handles no filesystem rights and only sets the
  abstract-socket and signal scopes. Because no filesystem access is
  handled, Landlock does not block its mount syscalls. Everything started
  later inherits the scopes.
- `init` is cloned with `CLONE_NEWPID | CLONE_INTO_CGROUP | CLONE_PIDFD`. It
  unshares mount and IPC namespaces (and network for `isolated` and `none`),
  builds the root on a detached tmpfs with `open_tree`, `fsopen`/`fsmount`,
  `move_mount`, and `mount_setattr`, pivots, hands the supervisor writable
  clones of the read-only root and `/dev` mounts, drops every capability,
  and reaps.
- A process that unshares its PID namespace can no longer create threads,
  so only the forker thread joins the sandbox PID namespace. Spawns are
  cloned there with `CLONE_INTO_CGROUP | CLONE_PIDFD`. The child joins the
  mount, IPC, and network namespaces through the init pidfd, starts a
  session, applies seccomp, and execs. The supervisor is the real parent of
  every app, so exit codes and signals pass straight through.
- The sandbox is idle when `workload/` is not populated and no client is
  connected; after `idle-timeout` seconds of that, the supervisor stops.

## Supervisors and units

Supervisors are started by systemd socket activation through two template
user units: `psbox@.socket`, listening on
`$XDG_RUNTIME_DIR/psbox/<key>.sock`, and `psbox@.service`
(`ExecStart=/usr/bin/psbox supervise %i`, `Delegate=yes`, `KillMode=mixed`).
`run` starts the socket unit over D-Bus when needed, and the first
connection starts the service. When a sandbox stops for being idle, its
socket keeps listening, so the next launch starts it again. `status`,
`mount`, and `stop` ask systemd whether the service is active first, so they
never start a sandbox.

Supervisor keys name sockets, units (`psbox@<key>`), and logs:

| Key | Supervisor of |
| --- | --- |
| `<sandbox>` | The sandbox's default instance |
| `<sandbox>.instance-<name>` | A named instance (`-i`) |
| `<sandbox>.task-<task>` | A task |
| `sandbox.<16 hex digits>` | An unnamed sandbox |

Supervisors get the user manager's environment (for example
`WAYLAND_DISPLAY`, imported by GNOME). Logs: `journalctl --user -u
psbox@<key>`.

Clients and supervisors talk over the `SOCK_SEQPACKET` socket: one JSON
message per packet, with file descriptors (the command's stdio) passed as
`SCM_RIGHTS`.

## Plans and the startup check

A sandbox config and the host's facts (paths, sockets, whether `pasta` is
installed) make a plan: the mounts, environment, commands, bus, display,
and network the sandbox gets. The client builds the plan before starting a
sandbox and runs the [startup check](cli.md#the-startup-check) on it; the
supervisor builds it again from the same config and repeats the check
before creating anything.

When the config changes, the supervisor builds a new plan and compares it
with the running one: mounts that can change live are attached and
detached, and anything else marks the sandbox for a restart when idle (see
[configuration.md](configuration.md#live-changes)).

## Live mounts

The supervisor clones the host tree (`open_tree`), creates the mount point
through the sandbox's own view (`/proc/<init>/root`) or the writable clones
of the root, and a short-lived helper joins the mount namespace and attaches
it with `move_mount`. The helper resolves every path it attaches to,
replaces, or detaches inside the sandbox's root without following symlinks,
so an app cannot redirect it by renaming directories. Detaching a mount
detaches what is mounted beneath it, so mounts beneath a changed one,
including live ones, are attached again with it.

Forwarded host sockets (Wayland, `pipewire-0`, `pulse/native`) are attached
when they first appear, and re-attached when their server creates them
again, with `MOVE_MOUNT_BENEATH` so the swap is atomic.

An overlay's upper and work directories live on a detached tmpfs that only
the overlay holds. After a task, `psbox task` sends each running instance of
the sandbox a refresh request naming the task's writable host directories;
the supervisor builds a new overlay over each matching lower directory,
attaches the mounts that belong beneath it inside that detached tree, and
swaps the whole tree in the same way; if any part fails, nothing changes.

## Devices

One inotify watch on `/run/udev/data`: a new or renamed entry means udev has
finished with the device. Matching devices are attached at
`/dev/.psbox/c<maj>-<min>` and then published by atomically renaming a
symlink into place (with their udev symlinks, such as `by-path` links), so
an app watching `/dev` never sees a half-ready node. The cloned node is
checked against the expected `st_rdev` before it is attached. Removed
devices are unpublished the same way. Devices are handled one at a time, so
the watcher and a config reload never act on the same device at once. A
device whose links cannot all be created is removed again; if its node
cannot be detached, the device stays recorded and the next event for it
tries again; until one succeeds, the sandbox restarts when idle, since the
node is still reachable.

## D-Bus

Each sandbox with `dbus = "portal"` runs a private `dbus-daemon` in the
`infra` cgroup with a psbox-written configuration. Its activatable services
are links to the host's `/usr/share/dbus-1/services` files minus portals and
`dbus-talk` names, plus the sandbox's own `~/.local/share/dbus-1/services`. A plain private bus
would activate `xdg-desktop-portal` inside the sandbox from the host's
service files; leaving portal services out makes portal names always reach
the gateway.

The supervisor's gateway owns `org.freedesktop.portal.Desktop` and
`org.freedesktop.Notifications` on that bus. Each sandbox client that calls
them gets its own host connection, which registers the client's app id
(from the command that started it) with
`org.freedesktop.host.portal.Registry`, so host services see per-client
lifetimes and portal request and session paths map one-to-one onto the
paths the client predicted from its own name. A client's host connection
closes when the client leaves the private bus, which ends its inhibitors
and sessions, except that it stays until an open OpenURI request is
answered: the host portal cancels a request whose caller disconnects, and
callers such as `gdbus call` exit as soon as they have the request, which
would close the app chooser (shown when no default app is set) before the
user picks an app. If the host portal goes away instead, the gateway ends
those requests with a `Response` of its own, so a client waiting for one is
not left hanging. The gateway reads the policy
from the current plan for each call, so device-class changes (Camera,
Realtime) apply live. The policy is in [isolation.md](isolation.md#d-bus).

GLib and `xdg-open` run a link's local handler before they try the
portal, and the host's desktop entries are visible in `/usr`, so a
sandboxed app would start the host's browser inside its own sandbox. With a
session bus, the root has the psbox executable (bound read-only at
`/run/psbox/bin/psbox`), and a `mimeapps.list` and desktop entry in
`/run/psbox` that make `psbox open-uri` the handler for the schemes
`[open-urls]` sends to the host; `XDG_CONFIG_DIRS` and `XDG_DATA_DIRS` find
them first. `psbox open-uri` calls `OpenURI.OpenURI` on the session bus
with the caller's `XDG_ACTIVATION_TOKEN` and waits for the request's
`Response` (with `dbus = "host"` it talks to the host portal directly, with
no gateway to keep the request open). The host portal opens the link with
the host's default app, which for a sandbox installed with `psbox install`
is `psbox run`.

The host portal identifies a client by the process that opened its host
connection and reads the process ids a client passes (Realtime) in that
process's PID namespace, as it does for Flatpak apps. So the connector opens
the host connections: a process forked on the forker thread into the
sandbox's PID namespace and the `infra` cgroup, started on first use, that
connects a socket to the host bus for each request and passes it back. It
joins the sandbox's mount namespace (and network namespace, when the sandbox
has one) and reaches the host bus through a descriptor for its socket
opened before the fork, so its `/proc` entries show only the sandbox's view.
It keeps the supervisor's capabilities, so sandbox processes, which have
none, cannot ptrace it or open its descriptors; the host portal, which runs
as the owner of the supervisor's user namespace, can read its `/proc` entry.
It starts with the signals that dump core blocked, ignores them before
unblocking them, and sets its core dump limit to the lower of its inherited
hard limit and one byte, so signals from the sandbox cannot make it dump the
supervisor's memory on the host. If it exits, does not answer
within five seconds, or finds its socket refusing connections (the host bus
made a new one), a new one is started for that client.

Each `dbus-talk` name gets its own gateway connection to the private bus,
which owns that name: GDBus proxies call the owner's unique name, which is
then unambiguous, and the bus accepts a reply only from the connection that
was called. Calls to it go out through the caller's host connection
unchanged.

Messages are written little-endian. A big-endian message from a client is
converted to little-endian when it is read; one whose body cannot be
decoded is skipped (with an error reply to a method call) without ending
the connection.

The host bus, for the gateway and for `dbus = "host"`, is `PSBOX_HOST_BUS`
when set (used for headless tests), else `DBUS_SESSION_BUS_ADDRESS`, else
`$XDG_RUNTIME_DIR/bus`. It must be a `unix:path=` address: the abstract-socket
scope means a host bus at an abstract address cannot be reached.

## Xwayland

With `xwayland` on (and a Wayland socket), the supervisor picks a display
number `N`, creates the sandbox's X socket itself as a filesystem socket at
`/tmp/.X11-unix/XN` (on a tmpfs of its own, even when `/tmp` is a share),
and sets `DISPLAY=:N` for everything it starts. The first connection starts
`xwayland-satellite :N -listenfd 3 -nolisten local` in the `infra` cgroup;
it runs a rootless Xwayland and turns each X11 window into a Wayland window
on the host compositor. If it exits, the next X client starts it again.

With `builtin`, satellite's `WAYLAND_DISPLAY` is
`/tmp/.X11-unix/psbox-wayland-N` instead of the host's socket, on the X
socket's own tmpfs, so it is never in a share. The supervisor accepts
satellite's connection there, closing any from another
process (by `SO_PEERCRED`), and starts `psbox decorations` in `infra` with
it, from the psbox bound at `/run/psbox/bin/psbox`. A ready-made socket
(`WAYLAND_SOCKET`) would not do: satellite starts Xwayland with its own
environment and descriptors before it connects, and Xwayland would take
the socket. When satellite exits, `psbox decorations` is stopped with it.

`psbox decorations` connects to the host compositor and passes satellite's
requests and events through, translating object ids between the two
connections. It offers satellite only the globals satellite uses, and when
the compositor has no `zxdg_decoration_manager_v1` (GNOME), its own. A
window that asks for server-side decorations gets a frame from
sctk-adwaita: subsurfaces of the window's surface on the proxy's own
connection, drawn before each commit of the window, with the window
geometry extended to take in the title bar. The proxy keeps the rest
consistent: configured sizes, size limits, and `configure_bounds` leave out
the title bar; popups are placed relative to the X11 window below it; and
pointer, touch, tablet, and drag events over a frame do not reach
satellite. The proxy's own seat drives the frames: its pointer moves,
resizes, maximizes, minimizes, and shows the window menu through the
window's `xdg_toplevel`, and the close button sends satellite the
toplevel's `close`. sctk-adwaita runs `dbus-send` (the color scheme, once
per proxy, and the button layout, from the settings portal, with a
100 ms timeout), `gsettings` (the title bar font), and `fc-match` inside
the sandbox, the last three for each new frame, and the proxy waits for
them; without them it falls back to a light theme, the default layout, and
its built-in font.

For `xwayland-scale`, the proxy rewrites the scale satellite sees, per
output. Satellite sizes the X screen from an output's current mode and
works at `wl_output.scale`, overridden by a window's
`wp_fractional_scale_v1.preferred_scale`. With `round-up`, the preferred
scale is rounded up to a whole one and the mode becomes the output's
logical size times that scale (2880x1920 at 150% becomes 3840x2560); with
`compositor`, both scales become 1 (and `wl_surface.preferred_buffer_scale`
too) and the mode becomes the logical size. The whole scale is the
compositor's `wl_output.scale`, and a mode that already is the logical size
times it stays as it is. The proxy holds the current mode until the
output's `done` (or, below xdg-output version 3, the end of a logical size
change), so it goes out with the scale and logical size of the same change,
and with it a `zxdg_output_v1.logical_size`, which is what satellite passes
on to Xwayland. The logical size is the one satellite's own
`zxdg_output_v1` reports, and until it has one, the one the proxy's own
connection reports (read before satellite's first request, and kept up to
date). A compositor without `zxdg_output_manager_v1` gets `native`, with a
line in the log. Title bars are drawn at the real scale.

X clients (libxcb, and Xlib through it) try the abstract socket
`@/tmp/.X11-unix/XN` before the file and fall back to the file only when
that name does not exist. With `network = "host"` the sandbox shares the
host's abstract names, and Landlock refuses connections to the host's X
servers, which clients treat as fatal. So `N` is the first number from 28787
(`0x7073`, "ps" in ASCII) up to 59535 that has no abstract X socket in
`/proc/net/unix` when the supervisor starts; reading it claims nothing on
the host. `status` shows it. A host X server started later on the same
number would break new X clients in that sandbox until it restarts.

## Network

`isolated` sandboxes get `pasta` attached to their network namespace:
outbound connections only (no port forwarding either way, no route to the
host's loopback), and DNS relayed to the host's resolver through
`169.254.1.1`, which a generated `/etc/resolv.conf` names. `pasta` stays on
the host side, in the supervisor's cgroup and mount namespace, and gets
`CAP_SYS_ADMIN` (to enter the sandbox's network namespace) and the network
capabilities it needs, in the supervisor's user namespace. `host` sandboxes
see the host's `/run/systemd/resolve` for name lookups.

## Source layout

| Module | Concern |
| --- | --- |
| `lib.rs`, `main.rs` | Command line and dispatch (`main.rs` only calls the library) |
| `cli.rs`, `ui.rs` | Reporting commands and terminal output |
| `client.rs`, `unnamed.rs` | The client side of `run`, `task`, and `sandbox` |
| `spec.rs`, `plan.rs` | Config parsing and validation; plans and the startup check |
| `supervisor.rs` | The supervisor: start, spawn, live changes, devices, idle |
| `root.rs`, `spawn.rs`, `forker.rs`, `inject.rs` | The sandbox root, spawning, and joining the sandbox's namespaces |
| `sys/` | Namespaces, the mount API, Landlock, seccomp, cgroups, inotify, file descriptors |
| `bus.rs`, `gateway.rs`, `connector.rs`, `portal.rs`, `dbus/` | The private bus, the gateway and its connector, the portal policy, and the D-Bus wire format |
| `devices.rs`, `x11.rs`, `desktop.rs`, `install.rs`, `systemd.rs`, `protocol.rs` | Device classes, Xwayland, desktop entries, the user manager, and the client protocol |
| `decorations/` | `psbox decorations`: the Wayland proxy between satellite and the compositor, and the title bars it draws |
| `fuzz.rs` | Entry points for the fuzz targets (built only for them) |
