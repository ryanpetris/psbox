# Glossary

These definitions are canonical for code, configuration, CLI output, and
documentation.

## Sandbox

A configured environment for one application or group of applications: a
private home, the host paths and devices it may use, its bus, display, and
network, and its apps. Each sandbox is defined by one sandbox config.

## Sandbox config

The Jsonnet file `$XDG_CONFIG_HOME/psbox/sandboxes/<name>.jsonnet`. The file
name is the sandbox name. psbox evaluates it on top of its base, checks the
result, and fills in every default.

## psbox library

What `import 'psbox'` returns in a config: the sandbox's name, home, root,
and storage, `psbox.path`, mount helpers, defaults, and built-in profiles.

## Sandbox name

Letters, digits, `_`, and `-`, never `.`, so that every `<name>.<type>`
directory in the sandbox root belongs to exactly one sandbox.

## Sandbox root

`$PSBOX_ROOT`, default `~/Sandbox`: where sandbox storage (`<root>/<name>`)
and other sandbox directories (`<root>/<name>.<type>`, such as
`~/Sandbox/retroarch.bin`) live. `psbox.root` in a config.

## Storage

A named sandbox's directory in the sandbox root, `<root>/<name>`
(`psbox.storage`), its default home. `psbox.storageFor(type)` names its
other directories, `<root>/<name>.<type>`.

## Home

The directory at `$HOME` inside the sandbox: a host directory bound
read-write (by default the sandbox's storage), an empty tmpfs (`mode:
'temporary'`), or none (`home: null`).

## Sandbox root filesystem

The root a sandbox's processes see, built from the plan on a tmpfs and
read-only once built. Not to be confused with the sandbox root directory.

## App

An `app.<name>` entry: one desktop entry. An application also says what
`psbox run --app <name>` starts (`run`) and with what environment; an
autostart entry starts an application at login; a suppressed entry hides a
host entry.

## Default app

The application named after the sandbox, which `psbox run <sandbox>` runs.
Without a config entry it runs the program of that name and inherits the
host's desktop entry with that id, if there is one.

## Launch

One `psbox run` (or task, or unnamed sandbox): the process it started and
everything that process starts, in the launch's own cgroup
(`workload/launch-<n>`), with the portal identity decided when it started.

## Instance

One running copy of a sandbox: its own supervisor, namespaces, and
processes. The default instance is named after the sandbox; named instances
(`psbox run <sandbox> -i <name>`) share the sandbox's home.

## Task

A `task.<name>` entry: a oneshot command run with `psbox task` in its own
instance, with the sandbox's config and the task's overrides, and stopped
as soon as it is done.

## Unnamed sandbox

A sandbox without a config, made by `psbox sandbox` from its options, that
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
apps, device classes, bus, display, and network. The client and the
supervisor build the same plan from the same config.

## Startup check

The check of a plan against the host before a sandbox starts, which reports
every problem that would stop it at once.

## Share

A host path made visible in the sandbox: a `read-write`, `read-only`, or
`overlay` mount.

## Mount

A `mount.<name>` entry, or anything psbox mounts in the sandbox root
filesystem. Mount modes are `read-write`, `read-only`, `overlay`, and
`empty`.

## Live mount

A mount attached to or detached from a running sandbox: by `psbox mount`,
by the file chooser, or by a config change.

## Device class

A named set of host devices a sandbox may use: `gpu`, `camera`,
`gamepad`, `security-key`, `alsa`, a USB device in `devices.usb`, or the
`audio` sockets.

## Published device

A device node of a granted class made visible in a running sandbox's `/dev`,
added and removed as the device comes and goes.

## Profile

A Jsonnet object a sandbox config imports and adds itself to: a user
profile (by convention in `$XDG_CONFIG_HOME/psbox/profiles/`), or a built-in
one such as `psbox.profiles.browser`.

## Private session bus

The `dbus-daemon` a sandbox with `dbus.mode: 'portal'` runs for its own
processes.

## Gateway

The part of the supervisor that serves the portals and notifications on the
private session bus, and the host names a sandbox lists in `dbus.talk`, and
forwards what the policy allows to the host.

## Connector

The process, in the sandbox's PID namespace, that opens the gateway's
connections to the host bus, so the host portal reads a client's process ids
in the sandbox's PID namespace.

## Link handler

The default app for links inside a sandbox with a session bus, unless
`links` keeps every link inside: psbox itself, as `psbox open-uri`, which
sends the `http`, `https`, and `mailto` links `links` does not keep in the
sandbox to the host's default app through the OpenURI portal.

## Decorations

`psbox decorations`, which in the builtin Xwayland mode sits between
`xwayland-satellite` and the compositor and draws the title bars of X11
windows when the compositor does not.

## Portal identity

The app id the gateway registers a launch's processes under with the host
portal: an application's `id` when the host portal accepts it (it has an
application entry with that id). A process without one reaches only the
portals that need none.

## Workload

The processes started for clients and their children, in the `workload/`
cgroup, one cgroup per launch. The sandbox is idle when the workload is
empty and no client is connected.

## Infra

The processes that serve the workload, each kind in its own cgroup beneath
`infra/`: init, the private session bus and the services it activates, the
display helpers (`xwayland-satellite`, Xwayland, and decorations), and the
connector.

## Network fallback

Using the host network for a sandbox whose network is `automatic`, because
`pasta` is not installed.

## Restart pending

A config change that cannot be applied live; the sandbox restarts as soon
as it is idle.

## Desktop entry

A `.desktop` file `psbox install` writes for an `app` entry, which starts
its application through `psbox run`.
