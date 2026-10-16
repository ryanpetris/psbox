# Isolation

What a process in a sandbox can and cannot reach. Sandboxes run as your own
user: psbox narrows what that user can see and do, it does not grant
anything the user could not do already.

## Filesystem

Each sandbox has its own root, built from its plan on an in-memory tmpfs
and read-only once built:

| Path | What the sandbox gets |
| --- | --- |
| `/usr`, `/etc`, `/opt`, `/sys` | The host's, read-only (`/opt` when it exists); `/bin`, `/sbin`, `/lib`, `/lib64` link into `/usr` |
| `/proc` | A new `proc` for the sandbox's own PID namespace |
| `/dev` | A read-only tmpfs with only `null`, `zero`, `full`, `random`, `urandom`, and `tty`, a private `devpts` and `/dev/shm`, and the devices its classes grant |
| `/tmp`, `/var/tmp` | Private tmpfs |
| `$XDG_RUNTIME_DIR` | A private tmpfs with only the forwarded sockets: Wayland, the audio sockets with `audio`, and the session bus |
| `~` | The private home (`<root>/<name>`), a host path, a tmpfs, or nothing |
| `~/.cache` | A tmpfs unless `cache = "home"` |
| `/run/udev`, `/var/cache/fontconfig` | The host's, read-only, for device and font metadata |
| Shares and mounts | Only what the config lists, read-only or read-write as configured |

Nothing else of the host's filesystem is visible: not your home, not other
sandboxes' homes, not `/run`, `/media`, or `/mnt`. psbox's mounts are locked
in any namespace an app creates inside the sandbox, so they cannot be
unmounted to reveal what they cover or remounted read-write.

## Namespaces and processes

- **User:** each supervisor has its own user namespace that maps only your
  uid and gid. Capabilities inside it apply to nothing on the host.
- **Mount, PID, IPC:** every sandbox has its own. Its processes see only
  each other, and MIT-SHM and System V IPC stay inside.
- **Network:** `isolated` and `none` sandboxes have their own network
  namespace; `host` sandboxes share the host's (see [Network](#network)).
- **Capabilities:** every process in the sandbox runs with none, and with
  `no_new_privs`, so setuid binaries and file capabilities gain nothing.
  The one exception is the gateway's
  [connector](architecture.md#d-bus), which appears in the sandbox's process
  list. It runs no program and only opens host bus connections, and it keeps
  the supervisor's capabilities and has no seccomp filter. Holding those
  capabilities is what stops sandbox processes from ptracing it or opening
  its descriptors. It shares the sandbox's mount and network namespaces, so
  its `/proc` entries show nothing the sandbox cannot already see.
- **seccomp:** a deny-list returns `EPERM` for the kernel keyring
  (`keyctl`, `add_key`, `request_key`), `kexec_load` and `kexec_file_load`,
  module loading and removal, `reboot`, `swapon`/`swapoff`, `acct`,
  `quotactl`, `perf_event_open`, `bpf`, and the terminal-injection ioctls
  `TIOCSTI` and `TIOCLINUX`. On x86_64 the x32 ABI is refused entirely, and
  any other architecture's system calls kill the process. The sandbox's own
  PID 1 has the same filter.

## Landlock scopes

The supervisor, and so every process in its sandbox, runs in a Landlock
domain with the abstract-socket and signal scopes. A sandboxed process
cannot connect to abstract Unix sockets created outside the sandbox (even
with `network = "host"`, which shares the host's abstract socket names) and
cannot signal processes outside it. Processes within one sandbox can still
reach each other's abstract sockets and signal each other.

The domain handles no filesystem rights, because a domain that does forbids
all mounting, even in namespaces the process creates itself. Apps can
therefore build their own sandboxes inside psbox: glycin's `bwrap` image
loaders (used by gdk-pixbuf), or Steam's runtime.

## Devices

A sandbox gets only the device nodes its classes match (see
[configuration.md](configuration.md#devices)), and the host's permissions
on each node still apply: a node your session cannot open is no more
accessible inside. Each node is checked against the expected device number
before it is published.

## D-Bus

With `dbus = "portal"` (the default), the sandbox has its own session bus.
Services activated on it (such as `dconf` and `gvfsd`) run inside the
sandbox and keep their state in the private home. Only the portals and
notifications reach the host, through the supervisor's gateway, plus any
names the config lists in `dbus-talk` (see below):

| Portal interfaces | Treatment |
| --- | --- |
| Account, Email, FileChooser, GlobalShortcuts, Inhibit, MemoryMonitor, NetworkMonitor, Notification, OpenURI, PowerProfileMonitor, Print, ProxyResolver, Secret, Settings, Trash | Forwarded |
| `OpenURI.OpenURI` | Only `http`, `https`, and `mailto` |
| `OpenURI.OpenFile` | Forced `ask`: the host always asks which app opens the file |
| Screenshot | Forced `interactive`; the image is made visible like a file-chooser result |
| ScreenCast | `persist_mode` and `restore_token` removed |
| FileChooser | Sandbox paths in options removed; the chosen files are made visible (existing writable shares are reused, otherwise the file is mounted at `$XDG_RUNTIME_DIR/doc/<n>/<name>`; save dialogs create the file first) |
| Camera | Only with the `camera` device class |
| Realtime | Only with the `audio` device class, and only `MakeThreadRealtimeWithPID` and `MakeThreadHighPriorityWithPID` for threads of the sandbox's own processes (the host portal looks the ids up in the sandbox's PID namespace, so no other process can be named) |
| Anything else (Background, DynamicLauncher, RemoteDesktop, Location, Wallpaper, Usb, Documents, ...) | Refused with `AccessDenied` and logged |

`org.freedesktop.Notifications` is forwarded with the `image-path` and
`sound-file` hints removed. Names listed in
[`dbus-talk`](configuration.md#host-bus-names) are reachable too, with
calls and replies passed unfiltered: listing one gives the sandbox that
service's full access, as yours. Every other host bus name is unreachable,
and the system bus is not available.

With `dbus = "host"`, the sandbox gets the host session bus and the system
bus unfiltered: every host service is reachable with your own permissions.
Use it only for apps that need it. With `dbus = "off"` there is no session
bus.

## Network

| `network` | Reach |
| --- | --- |
| `isolated` | Outbound connections and DNS through `pasta`. No incoming connections, no route to the host's loopback services, no host abstract sockets |
| `host` | The host's network, including services listening on `localhost`. Host abstract sockets stay unreachable |
| `none` | Loopback only |

`pasta` runs on the host side, attached to the sandbox's network namespace,
with `CAP_SYS_ADMIN` (to enter that namespace) and the network capabilities
it needs, all in psbox's user namespace, so they apply to nothing on the
host.

## Display

The sandbox gets the host's Wayland socket, so apps draw through the
compositor like any Wayland client. There is no access to a host X server:
X11 apps get a private Xwayland per sandbox (`xwayland = true`).

## Not isolated

- **Audio:** `audio` passes the host PipeWire and PulseAudio sockets through
  unfiltered. A sandbox with `audio` can play, record from microphones and
  other apps' output, open PipeWire camera nodes, and load `pipewire-pulse`
  modules.
- **System files:** `/usr`, `/etc`, and `/sys` are readable, as for any
  process of your user.
- **Shares:** anything listed in `rw`, `ro`, or a mount is readable (and
  writable if so configured) with your own permissions.
- **`dbus = "host"` and `network = "host"`:** as described above.
