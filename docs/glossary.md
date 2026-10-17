# Glossary

These definitions are canonical for code, configuration, CLI output, and
documentation.

## Sandbox

A configured environment for one application or group of applications: a
private home, the host paths and devices it may use, its bus, display, and
network, and its commands. Each sandbox is defined by one sandbox config.

## Sandbox config

The TOML file `$XDG_CONFIG_HOME/psbox/sandboxes/<name>.toml`. The file name
is the sandbox name.

## Sandbox name

Letters, digits, `_`, and `-`, never `.`, so that every `<name>.<type>`
directory in the sandbox root belongs to exactly one sandbox.

## Sandbox root

`$PSBOX_ROOT`, default `~/Sandbox`: where private homes (`<root>/<name>`)
and other sandbox directories (`<root>/<name>.<type>`, such as
`~/Sandbox/retroarch.bin`) live. `@root` and `@self` refer to it.

## Private home

The directory bound at `$HOME` inside the sandbox: `<root>/<name>` by
default, or a host path. `home = "tmpfs"` gives an empty in-memory home and
`home = "none"` no home.

## Sandbox root filesystem

The root a sandbox's processes see, built from the plan on a tmpfs and
read-only once built. Not to be confused with the sandbox root directory.

## Command

A `[command.<name>]` entry: what `psbox run` starts in the sandbox (`exec`),
with its environment, app id, and desktop entry.

## Instance

One running copy of a sandbox: its own supervisor, namespaces, and
processes. The default instance is named after the sandbox; named instances
(`psbox run <sandbox> -i <name>`) share the sandbox's home.

## Task

A `[task.<name>]` entry: a oneshot command run with `psbox task` in its own
instance, with the sandbox's mounts, and stopped as soon as it is done.

## Unnamed sandbox

A sandbox without a config, made by `psbox sandbox` from its flags, that
ends when its command exits.

## Supervisor

`psbox supervise <key>`, started by systemd socket activation: the process
that owns one instance's namespaces and cgroups, builds its root, spawns its
processes, applies live changes, publishes devices, and runs its gateway.

## Key

The name of one supervisor, used for its socket, its units
(`psbox@<key>.socket` and `psbox@<key>.service`), and its logs: `<sandbox>`,
`<sandbox>.instance-<name>`, `<sandbox>.task-<task>`, or
`sandbox.<16 hex digits>` for an unnamed sandbox.

## Plan

What a sandbox config resolves to on this host: the mounts, environment,
commands, device classes, bus, display, and network. The client and the
supervisor build the same plan from the same config.

## Startup check

The check of a plan against the host before a sandbox starts, which reports
every problem that would stop it at once.

## Share

A host path made visible in the sandbox: an `rw` or `ro` entry, or a `ro`,
`rw`, or `overlay` mount.

## Mount

A `[mount.<name>]` entry, or anything psbox mounts in the sandbox root
filesystem. Mount modes are `ro`, `rw`, `overlay`, and `empty`.

## Live mount

A mount attached to or detached from a running sandbox: by `psbox mount`,
by the file chooser, or by a config change.

## Device class

A named set of host devices a sandbox may use, such as `gpu`, `camera`,
`gamepad`, `security-key`, `alsa`, `usb:<vid>:<pid>`, or the `audio`
sockets.

## Published device

A device node of a granted class made visible in a running sandbox's `/dev`,
added and removed as the device comes and goes.

## Profile

A config fragment merged under a sandbox config with `extends`: a user
profile in `$XDG_CONFIG_HOME/psbox/profiles/`, or the built-in `browser`
profile.

## Private session bus

The `dbus-daemon` a sandbox with `dbus = "portal"` runs for its own
processes.

## Gateway

The part of the supervisor that serves the portals and notifications on the
private session bus, and the host names a sandbox lists in `dbus-talk`, and
forwards what the policy allows to the host.

## Connector

The process, in the sandbox's PID namespace, that opens the gateway's
connections to the host bus, so the host portal reads a client's process ids
in the sandbox's PID namespace.

## Link handler

The default app for links inside a sandbox with a session bus, unless
`[open-urls]` keeps every link inside: psbox itself, as `psbox open-uri`,
which sends the `http`, `https`, and `mailto` links `[open-urls]` does not
keep in the sandbox to the host's default app through the OpenURI portal.

## App id

The desktop file id a process is registered with at the host portal: the
command's `app-id`, else the sandbox's.

## Workload

The processes started for clients and their children, in the `workload/`
cgroup. The sandbox is idle when the workload is empty and no client is
connected.

## Infra

The processes that serve the workload: init, the private session bus and
the services it activates, and Xwayland, in the `infra/` cgroup.

## Network fallback

Using the host network for a sandbox that does not set `network`, because
`pasta` is not installed.

## Restart pending

A config change that cannot be applied live; the sandbox restarts as soon
as it is idle.

## Desktop entry

A `.desktop` file `psbox install` writes for a command with a `desktop`
table, or for an `autostart` table, which starts the command through `psbox
run`.
