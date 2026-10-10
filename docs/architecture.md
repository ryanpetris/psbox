# Architecture

Three binaries, one objects directory, one composed `bwrap` per sandbox.

```text
desktop / shell
       |
       v
     psbox          host CLI (launch, instance, objects, sandbox)
       |
       +-- oneshot: exec bwrap --> workload
       |
       +-- instance: start psboxd@<id>.socket (systemd user D-Bus)
                         |
                         v
                       psboxd        host daemon (socket activated)
                         |
                         +-- bwrap --> psboxa (PID 2)
                         |               |
                         |               +-- optional dbus-daemon
                         |               +-- spawn workload
                         |               +-- xdg-open helper
                         |
                         +-- host xdg-open (http/https only)
```

`psbox launch` never execs `psbox sandbox`. Both call the same
in-process compose. `psboxd` execs `bwrap` and `psboxa`. `psboxa` execs
the workload. Those are not self-calls.

## Compose

Every sandbox gets `--die-with-parent`, `--unshare-pid`, and
`--new-session`, a fresh `/proc` and `/tmp`, and read-only
tries of `/usr`, `/etc`, `/opt`, `/sys`, plus usrmerge symlinks for
`/bin`, `/sbin`, `/lib`, and `/lib64`.

`options.dev` defaults to `private`: a fresh `/dev` with private shared
memory and pseudo-terminals plus selective device binds. `true`/`host`
binds host `/dev` instead. `private-shm` binds host `/dev`, then mounts a
private tmpfs with mode `1777` over `/dev/shm`. Both host modes share host
pseudo-terminals and skip selective device binds while retaining audio and
display socket integration. Device exclusions are rejected for host modes.

`$XDG_RUNTIME_DIR` is a tmpfs. Selected sockets are bound back in:
Wayland, Pulse, PipeWire, and D-Bus sockets when enabled. `dbus:
private` and `dbus: false` bind no host bus sockets. See
`internal/bwrap/compose.go` for the flag list.

The private home is a read-write bind of `$PSBOX_ROOT/<sandbox>` (or
`:tmpfs:` / `:none:`). A real home bind also mounts `$HOME/.cache` as
tmpfs unless `options.cache` is `home`. `downloads: true` binds the
host `Downloads` directory. YAML `bwrap-args` are appended after that.

Instance trailing command is `psboxa`, with `PSBOX_CONTROL_FD`
pointing at the daemon SEQPACKET socket (extra file descriptor 3).
When `host-urls` is on (the default), compose also installs a `PATH`
symlink for `xdg-open`. Oneshot trailing command is the workload,
optionally prefixed with `dbus-launch`.

## Display and environment

The sandbox inherits the launching client's `WAYLAND_DISPLAY` and
`XAUTHORITY` at compose time (instance: from the spawn message; oneshot:
from the `psbox` process). `psboxa` forces those two keys, plus
`XDG_RUNTIME_DIR`, onto every spawned workload so they match the binds.

`dbus: private` sets `DBUS_SESSION_BUS_ADDRESS` to the in-sandbox bus
and drops `DBUS_SESSION_BUS_PID` / `DBUS_SESSION_BUS_WINDOWID`.

The agent searches the workload `PATH` for executable files. Relative paths
and relative `PATH` entries resolve against the workload working directory.
A failed search returns an error; the working directory is searched only
when explicitly present in `PATH` or in the command path.

## Logging

JSON on stderr from `psboxd` and `psboxa`. `psboxa` logs when a
workload command starts and exits; `command` is `argv[0]` only. The
CLI prints only the log message on stderr. Set `PSBOX_LOG=structured`
for slog text (time, level, message, and attributes).

## Packaging and objects

Distro recipes consume the in-tree `systemd/` units and the three
binaries. Object collections for real applications live with the
packager.

## Device manual validation

See [AGENTS.md](../AGENTS.md#build-test-and-release) for the development
checks and analysis gates.

Device mount checks run manually on a Linux host. They require a working
Bubblewrap environment; the automated tests validate parsing, composition,
and configuration hashes without requiring nested sandboxes.

Host manual checks for device modes:

1. Create a temporary marker in host `/dev/shm`. Confirm it is visible with
   `psbox sandbox --no-home --dev host -- ls /dev/shm`, and hidden with
   `--dev private` and `--dev private-shm`. Remove the marker afterward.
2. Check `stat -c %a /dev/shm` inside `--dev private-shm`; it must print
   `1777`.
3. Compare host `/dev/pts` with each sandbox mode. `host` and `private-shm`
   expose host terminal nodes; `private` has its own terminal filesystem.
4. Keep a sandbox running with `host` or `private-shm`, then connect a
   hidraw device. Confirm the new `/dev/hidraw*` path appears and follows
   host access permissions.
