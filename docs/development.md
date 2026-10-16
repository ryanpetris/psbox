# Development

## Build and check

Run from the repository root:

```sh
make build          # debug build
make test           # cargo test
make fmt            # cargo fmt
make check          # fmt check, clippy (native, amd64 and arm64 musl), tests, cargo-deny
make release        # static release binaries for both targets
make dev/install    # debug build plus user units pointing at it
```

`make check` must pass before a change is complete. It needs the
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` targets
(`rustup target add ...`) and `cargo-deny` (`cargo install --locked
cargo-deny`). Both musl targets link with Rust's own `rust-lld`
(`.cargo/config.toml`), so no cross C toolchain is needed.

`make dev/install` installs `psbox@.socket` and `psbox@.service` into
`$XDG_CONFIG_HOME/systemd/user` with `ExecStart` pointing at
`target/debug/psbox`, overriding packaged units, and reloads the user
manager. After moving the checkout, run it again, and `target/debug/psbox
install` to rewrite desktop entries that name the old path.

## Fuzzing

`fuzz/` is a separate crate (its own workspace) with libFuzzer targets for
the parsers that read input psbox does not control:

| Target | Input |
| --- | --- |
| `config` | A sandbox config, and the plan built from it |
| `protocol` | A request or response on a supervisor socket |
| `dbus_message` | A D-Bus message from a sandbox client: parsed, its arguments decoded, checked against the portal policy, and encoded again |
| `desktop_entry` | A host desktop entry, rewritten as `install` does |

The targets call `psbox::fuzz`, which is compiled only with `--cfg fuzzing`
(set by cargo-fuzz). With a nightly toolchain and `cargo-fuzz`:

```sh
make fuzz FUZZ_TIME=300                       # each target for 300 s
cd fuzz && cargo +nightly fuzz run dbus_message corpus/dbus_message
```

A crash leaves its input in `fuzz/artifacts/<target>/`; add it as a
regression test beside the code it broke.

## GUI tests without a session

`scripts/headless-gnome.sh` runs GNOME Shell headless with its own Wayland
socket, a private session bus, in-memory GSettings, and temporary XDG
directories, so it never touches a real session or settings.
`scripts/gnome-shell-probe.py` takes screenshots and lists windows as the
shell sees them (it claims an allowlisted name on that private bus).

Supervisors take their environment from the user manager, so point it at the
test instance while testing:

```sh
scripts/headless-gnome.sh psbox-test-0 &        # prints the bus file path
systemctl --user set-environment WAYLAND_DISPLAY=psbox-test-0 PSBOX_HOST_BUS="$(cat /var/tmp/psbox-gnome.XXXXXX/bus)"
psbox run demo -c calc &
scripts/gnome-shell-probe.py /var/tmp/psbox-gnome.XXXXXX/bus windows
scripts/gnome-shell-probe.py /var/tmp/psbox-gnome.XXXXXX/bus shot /tmp/shot.png
systemctl --user unset-environment WAYLAND_DISPLAY PSBOX_HOST_BUS
```

## Verified behavior

Verified on Linux 7.2 with GNOME, beyond the unit tests:

| Area | Result |
| --- | --- |
| Root built from a plan: read-only `/usr`, `/etc`, `/sys`; private `/tmp`, `/var/tmp`, runtime dir; private home | Works |
| `/dev`: read-only tmpfs with base nodes, private `devpts` and `/dev/shm` | Works; `touch /dev/x` gives `EROFS` |
| Mount modes `ro`, `rw`, `overlay` (writes discarded), `empty` (mask) | Work |
| Agentless spawn: the supervisor is the real parent; exit codes and signals pass through | Works |
| Spawned processes: no capabilities, `NoNewPrivs`, seccomp filter | Works |
| `psbox sandbox`: tmpfs or given home, `--bind SRC:DST`, `--ro-bind`, `--env`, `--no-display`, exit status, no units or settings left behind | Works |
| Startup check: every missing path, setting, and config error at once; `--create-dirs` and directories created in the sandbox root | Works |
| `dbus = "host"`: host session and system buses reachable | Works |
| Nested `bwrap` (glycin image loaders through gdk-pixbuf); psbox mounts stay locked in nested namespaces | Works |
| Landlock abstract-socket scope: host abstract sockets unreachable | `EPERM` |
| Processes started separately in one sandbox reach each other's abstract sockets and signal each other | Works |
| Live mount and unmount, seen by an already-running process | Works |
| GPU render nodes published through `/dev/.psbox` and symlinks (including `by-path` links) | Works |
| Device hotplug: a security key plugged in while a browser runs appears in the sandbox and works for passkeys | Works |
| Idle stop after `idle-timeout` with no workload; socket kept for the next launch | Works |
| GTK 4 apps on headless GNOME Shell through the forwarded Wayland socket, rendering on the GPU | Works |
| Private session bus per sandbox; portal services not activated inside | Works |
| GNOME app identity: GTK apps report their real app id and group under their desktop entries | Works |
| Gateway: Settings, Inhibit, notifications (tagged with the app id), file chooser (a host file mounted at `$XDG_RUNTIME_DIR/doc/1/<name>`) | Work |
| Gateway: `Background`, `DynamicLauncher`, `file://` in `OpenURI`, other bus names | Refused and logged |
| Socket-activated supervisors: first start, concurrent first starts, idle stop, re-activation, `status`, `stop`, `stop --all` (including stray unnamed socket units) | Work |
| `psbox check <sandbox>`: the sandbox and its tasks checked without starting anything | Works |
| `install` with a missing `sandboxes` directory (removes its entries) and with one that is unreadable or a file (an error, nothing changes) | Works |
| Shares through symlinks (read-write, read-only, overlay, live mount); a symlink to a missing path reported | Works |
| Network modes `isolated` (DNS and HTTPS; host loopback unreachable; gigabit throughput through pasta), `none`, `host` | Work |
| Network default with and without pasta: `isolated`; `host (no pasta)`, the `check` warning, and the startup report | Works |
| Live config: share added and removed under a running process; env change on the next process; `home` change restarts when idle and warns while busy; a live mount beneath a changed share attached again | Works |
| Forwarded socket appearing late, and replaced by a restarted server | Re-attached |
| Tasks and named instances | Work; tasks stop when done and return the exit code |
| Desktop entries copied and inline, launched with `gio launch` | Grouped under their entries in GNOME |
| Xwayland on demand: `glxgears`, `xdpyinfo`, GTK apps with `GDK_BACKEND=x11`, with the host network while the host's Xwayland holds `:0` and `:1` | Work on display `:28787`; no abstract sockets |
| Static builds for amd64 and arm64 (arm64 run under qemu) | Work |
| Overlay refresh after an update task: a running instance sees the new version on a fresh overlay, while a process whose working directory is in the old overlay keeps the old files | Works |
| `profiles/` created, removed, and created again while a sandbox runs | Reloads each time |
| Gateway against two private `dbus-daemon`s: a `dbus-talk` name called by name and by its unique name, an unlisted name unreachable, a big-endian call served and the gateway still serving afterwards | Works |
| `dbus-talk` on the session bus: a host name called by name and by its unique name, alongside the portal; an unlisted name never reaches the host | Works |
| Realtime with `audio`: `MakeThreadRealtimeWithPID` and `MakeThreadHighPriorityWithPID` on sandbox processes; an id not in the sandbox refused by the host portal | Works (`SCHED_RR` and nice -10 on the host) |
| The connector from inside the sandbox: ptrace, `pidfd_getfd`, and its `fd`, `maps`, `root`, and `environ` entries | Refused |
| The connector's `mountinfo` and `net/tcp` from inside the sandbox | The sandbox's own |
| The connector killed with `SIGQUIT`, or stopped with `SIGSTOP`, from inside the sandbox | No core dump; reaped, and replaced on the next call (after five seconds when stopped) |
| Portal methods from inside a sandbox: `Settings.ReadOne`, `OpenURI.SchemeSupported`, `NetworkMonitor.GetConnectivity`, a property read; an unlisted `Settings.Write` | Forwarded; the unlisted method refused and logged |
| `psbox check` on Linux 7.2 | Kernel and user namespaces reported from the real kernel and a real namespace |

## Host checklist

Run on a GNOME session after changes to device handling:

1. Plug in a gamepad, camera, or security key with its class enabled: the
   node appears in the running sandbox (`psbox status <name>`), and
   disappears when unplugged.
