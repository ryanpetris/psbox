# Instances

Instance isolation is the default. One systemd user socket, one
`psboxd`, one `bwrap`+`psboxa`, many attaches.

## Identity

Identity is `<sandbox>/<instance>`. The sandbox name is the target
application (after `options.sandbox:`). The instance name defaults to
`default`.

systemd `%i` is that string escaped the same way `systemd-escape` does:
`/` becomes `-`. `firefox/default` is unit `psboxd@firefox-default`.

The listen path is `$XDG_RUNTIME_DIR/psbox/<escaped>.sock`. Mode `0600`.
The path must fit in `sockaddr_un.sun_path` (107 bytes plus NUL).

## Startup

1. `psbox` starts `psboxd@<escaped>.socket` through the systemd user
   manager. It uses the session bus first (`DBUS_SESSION_BUS_ADDRESS`
   or `$XDG_RUNTIME_DIR/bus`, same as `systemctl --user`). If that
   bus is missing, it falls back to `$XDG_RUNTIME_DIR/systemd/private`.
2. It connects with `unixpacket` (SEQPACKET).
3. It sends one `spawn` JSON frame and passes stdin, stdout, and stderr
   with `SCM_RIGHTS`.
4. `psboxd` ensures the sandbox (create, join, or idle rebuild),
   forwards the spawn to `psboxa`, and relays `spawned` / `exited` /
   `spawn_error`.
5. Signals (`SIGINT`, `SIGTERM`, `SIGHUP`, `SIGQUIT`, `SIGTSTP`,
   `SIGCONT`) are sent as `signal` frames. `SIGTSTP` also stops the
   CLI itself.

The CLI starts only that socket unit. systemd socket-activates
`psboxd@.service`. A failed connect retries the socket start; it does
not start the service.

Talking to the user manager uses the session bus when it is
available. On that bus, `psbox` calls `Hello`, `Subscribe`, and
`AddMatch` for `JobRemoved`. The private socket is AUTH only (no
`Hello`, no match rules). Each method call has a 2s deadline.
`StartUnit` / `StopUnit` wait at most 5s for `JobRemoved`. Dial,
AUTH, `Hello`, and `Subscribe` share a 5s deadline. List and stop
retry a timed-out or disconnected call up to three times and drop
the D-Bus connection between tries. Start retries only while the
unit is not yet active. After a failed stop, success requires a confirmed
`inactive`, `failed`, or missing unit; a failed state query remains an error.
Setup deadlines and cancellation cover filesystem and abstract Unix sockets,
TCP, and nonce TCP addresses, including authentication.

The first protocol reply must arrive within 5 seconds. The spawn
frame must be at most 1 MiB. An oversized environment is rejected
with the largest variable names in the error. Malformed or truncated frames
and excess descriptors are rejected, with received descriptors closed.
Protocol writes are bounded to 5 seconds; a stalled write closes its connection.

`--objects` cannot point at a different collection than the process
default. The daemon loads objects using its own process environment.

## Hashes

Each sandbox has a **config hash** and a **running hash** (blake2b-512).

Config hash includes home, non-default audio/video/display/fontconfig,
non-host dbus, `host-urls: false`, `cache: home`, `downloads: true`,
and `bwrap-args`. It does not include isolation, `exec`,
`default-args`, `env`, or `metadata.type`. Type defaults are hashed as
the options they fill.

Running hash is the config hash plus non-default `WAYLAND_DISPLAY`,
`XAUTHORITY`, `PIPEWIRE_CORE`, `PSBOX_ROOT`, and `PSBOX_OBJECT_PATH`.

Before each launch, `psboxd` reloads and validates the sandbox application and
collection options. Invalid or missing sandbox applications fail that launch.
If hashes differ and a workload is present, `psboxd` joins and logs a
warning, retaining the running configuration and hashes. If hashes differ
and the instance is idle, it tears down `bwrap`
and starts again using the reloaded configuration. Removing a collection
`sandbox_root` override restores the process default on the next idle rebuild.
A hash mismatch alone does not fail a launch. The client
warns when the hashes in `spawned` differ from the hashes it computed.

## Liveness and idle

`psboxa` treats the private `dbus-daemon` as infrastructure. A process
first seen as its child stays infrastructure after it reparents to
PID 1. Processes with `DBUS_STARTER_ADDRESS` or
`DBUS_STARTER_BUS_TYPE` (D-Bus activation) are infrastructure too.
Other processes are workload. After 60 seconds with no workload it
exits.

`psboxd` polls accept with a 1 second deadline. If `bwrap` is gone and
no clients remain, `psboxd` exits. The socket unit stays and will start
a new service on the next connect. If the agent disconnects, outstanding
clients receive an error even after `spawned`, and the daemon terminates and
reaps that sandbox. Each process has one waiter. Agent shutdown kills and
reaps tracked workloads and stops its private bus; unexpected private-bus
termination fails the agent through the same cleanup path.

`StartLimitBurst=5` is set on the service. The socket sets
`TriggerLimitIntervalSec=0`.

## List and stop

`psbox instance list` shows instantiated units (including idle sockets
with no service). The default instance is listed as the sandbox name
(`firefox`), not `firefox/default`. Extra columns (`PID`, `SINCE`,
`RESTARTS`, `ACCEPTED`) are systemd unit properties: `MainPID` is
`psboxd`, not the workload. `psbox instance stop` / `stop --all` stop
the service and then the socket so the instance does not come back
until the next launch. `stop` still accepts `firefox/default`. See
[cli.md](cli.md).

## Protocol

Version `1`. JSON objects on `SOCK_SEQPACKET` with optional fds.

| `type` | Direction | Role |
| --- | --- | --- |
| `spawn` | client to daemon to agent | argv, env, cwd, stdio fds |
| `spawned` | agent to client | pid, hashes from the daemon |
| `spawn_error` | daemon/agent to client | spawn or sandbox failure, including after `spawned`; optional errno and hashes |
| `exited` | agent to client | `code` and/or `signal` |
| `signal` | client to agent | `signum` delivered to the spawn pgid |
| `query_liveness` | daemon to agent | idle rebuild check |
| `liveness` | agent to daemon | `workload` bool |
| `open_uri` | agent to daemon | `uri` |
| `open_uri_result` | daemon to agent | `ok`, `message` |

The daemon owns `psboxd`. The agent is `psboxa`. The CLI is the client.

## Shared home

Named instances of one sandbox share `options.home`. They do not share
a `bwrap` namespace. `firefox/default` and `firefox/work` are two
daemons and two agents.

## Host integration checks

Use a disposable application and instance on a Linux test host with the current
binaries, an accessible systemd user manager, and the psbox user units installed.

1. Launch a short command and verify its output and exit status. Repeat with
   `dbus: private` and a command that connects to the private session bus.
2. Keep a workload running, change a sandbox option, and verify another launch
   joins with a hash warning. Once idle, launch again and verify the new setting
   takes effect. Repeat with a collection `sandbox_root` change and its removal.
3. Terminate the test instance's agent after a workload is acknowledged. Verify
   the client fails promptly, the sandbox terminates, and the daemon exits once
   its clients disconnect.
4. Stop the test instance through `psbox instance stop` and verify both its
   service and socket stop. Restore the disposable configuration afterwards.
