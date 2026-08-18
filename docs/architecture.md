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
`--new-session`, a fresh `/proc`, `/dev`, and `/tmp`, and read-only
tries of `/usr`, `/etc`, `/opt`, `/sys`, plus usrmerge symlinks for
`/bin`, `/sbin`, `/lib`, and `/lib64`.

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

## Logging

JSON on stderr from `psboxd` and `psboxa`. The CLI prints only the log
message on stderr. Set `PSBOX_LOG=structured` for slog text (time,
level, message, and attributes).

## Packaging and objects

Distro recipes consume the in-tree `systemd/` units and the three
binaries. Object collections for real applications live with the
packager.
