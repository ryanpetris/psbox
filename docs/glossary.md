# Glossary

These definitions are canonical for code, configuration, CLI output, and
documentation.

## Application

A named `kind: Application` object. It has an `exec`, optional
`default-args`, `env`, `bwrap-args`, and `options`. The name is
`metadata.name` or, when a file contains exactly one application, the
file stem. `metadata.type` may fill unset options (see
[configuration.md](configuration.md)).

## Sandbox

The application that owns the private home and the composed `bwrap`
flags. For a referrer (`options.sandbox: other`), the sandbox is
`other`, not the referrer. Instance identity keys on the sandbox name.

## Referrer

An application that sets `options.sandbox` to another application. It
supplies `exec`, `default-args`, and `env`. It must not set home, audio,
video, dbus, display, fontconfig, isolation, `host-urls`, `cache`,
`downloads`, `metadata.type`, or `bwrap-args`.

## Isolation

How a launch enters a sandbox. Values are `oneshot`, `instance`, or
`instance:<name>`. Omitted isolation is `instance` with instance name
`default`.

## Oneshot

Isolation that `exec`s `bwrap` with the workload as the initial command.
There is no `psboxd`, no `psboxa`, and no host URL forward. The sandbox
ends when that process tree ends.

## Instance isolation

Isolation that joins or creates a long-lived named instance. `psboxd`
owns the listen socket. `bwrap` runs `psboxa`. Later launches attach
over the instance protocol.

## Instance

One named long-lived sandbox of one sandbox application. The default
name is `default`. Identity is `<sandbox>/<instance>`, for example
`firefox/default`. Concurrent names of the same sandbox share the
private home.

## Object

One YAML document under the objects directory. Kinds are `Application`,
`FreedesktopEntry`, `FreedesktopAutostartEntry`, and `Options`.

## Objects directory

The directory `psbox` loads. Default `$XDG_CONFIG_HOME/psbox/objects`.
Override with `PSBOX_OBJECT_PATH` or `--objects`. `--objects` cannot be
used with instance isolation when it differs from the process default.

## Private home

The directory bound to `$HOME` inside the sandbox. Default
`$PSBOX_ROOT/<sandbox>` (`~/Sandbox/<sandbox>`). `:tmpfs:` is an empty
tmpfs. `:none:` binds no home.

## Workload

An application process started by a spawn, plus any descendants that
are not infrastructure. The private session `dbus-daemon` is
infrastructure, as are processes it activated (including after they
reparent to PID 1). Those do not keep an idle instance alive by
themselves.

## `psbox`

The host CLI. Applications start with `psbox launch <application>`.

## `psboxd`

The per-instance host daemon. systemd user socket activation starts
`psboxd@<escaped>.service`. It composes `bwrap`, talks to `psboxa`, and
runs host `xdg-open` for allowlisted URIs.

## `psboxa`

The in-sandbox agent (PID 2 under `bwrap` in instance isolation). It
spawns the workload, reaps it, may start a private session bus, and
forwards `http`/`https` to `psboxd`. Invoked as `xdg-open` or
`psboxa xdg-open`, it is the host-URL helper.

## Host URL forward

The instance-only path that sends `http` and `https` from the sandbox
to host `xdg-open`. Other schemes stay inside. `options.host-urls:
false` disables it.

## Config hash

A blake2b-512 digest of the sandbox options that affect the instance
root: home, non-default audio/video/display/fontconfig, non-host dbus,
`host-urls: false`, `cache: home`, `downloads: true`, and `bwrap-args`.
Isolation, `exec`, `default-args`, `env`, and `metadata.type` are not
included; type defaults are hashed as the options they fill.

## Running hash

A blake2b-512 digest of the config hash plus non-default compose
environment (`WAYLAND_DISPLAY`, `XAUTHORITY`, `PIPEWIRE_CORE`,
`PSBOX_ROOT`, `PSBOX_OBJECT_PATH`).

## Launch (verb)

The CLI action that starts an application in whichever isolation that
application uses. It is not an isolation mode.
