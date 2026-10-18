# CLI

```sh
psbox run <sandbox> [-i INSTANCE] [--app NAME] [--exec] [--create-dirs] [-- ARGS...]
psbox task <sandbox> <task> [--create-dirs] [-- ARGS...]
psbox sandbox [OPTIONS] [--create-dirs] -- COMMAND...
psbox list [--json]                     # sandboxes, what runs, their network, apps, and tasks
psbox status <sandbox> [-i INSTANCE] [--json]   # processes, mounts, devices
psbox config <sandbox> [--task TASK]    # the config as psbox sees it
psbox mount <sandbox> [-i INSTANCE] <host-path> [--at PATH] [--ro]
psbox umount <sandbox> [-i INSTANCE] <at>
psbox stop <sandbox> [-i INSTANCE] | --all
psbox install [--dry-run] [-v]          # desktop and autostart entries
psbox devices [--all] [--json]          # devices the classes match
psbox check [SANDBOX]                   # what this system provides and lacks, or whether a sandbox can start
psbox completions bash|zsh
```

Every command takes `--color auto|always|never`, `--help`, and the top level
takes `--version`.

## Running

`psbox run <sandbox>` starts the sandbox when it is not running (through its
socket unit) and runs an app in it; the app's stdin, stdout, and stderr are
the caller's, signals are forwarded, and its exit status is returned.
`--app` picks an `app.<name>` application; without it, the default app (the
one named after the sandbox) runs. Arguments after `--` are appended to the
app's `run`. `--exec` treats them as the whole command line instead (the
app, or without `--app` the default app, still supplies its environment and
[portal identity](configuration.md#portal-identity)). `-i` starts or joins
a named instance.

Apps start in the caller's working directory when the sandbox has it (as
with a mount of `~/Projects` and a caller in `~/Projects/x`), else in the
home, else in `/`.

`psbox task <sandbox> <task>` runs a task in its own supervisor and waits
for it. Afterwards, running instances of the sandbox replace their overlays
over what the task could write, so they see what it changed (see
[configuration.md](configuration.md#instances-and-tasks)); an instance that
cannot is named in a warning, and keeps the old files until it restarts.
`--create-dirs` on `run`, `task`, and `sandbox` also creates the missing
sources of mounts with `create: true` outside the sandbox root, and
confirms each one (see [configuration.md](configuration.md#paths)). Other
sources are never created; the startup check says so when `--create-dirs`
was given.

`run`, `task`, and `sandbox` print nothing of their own (stdio belongs to
the command), only warnings: a sandbox on the host-network fallback, one
still running an older config, or an app running without a portal
identity.

## The startup check

Before a sandbox starts, `run`, `task`, and `sandbox` check everything known
to stop it, and list every problem at once, by section, instead of failing
on the first:

- every config error: a Jsonnet error by file, line, and column, and every
  validation error by key path;
- shared host paths that do not exist, named by their `mount.<name>` entry,
  unless the mount is `optional` or psbox creates the path; a missing path
  inside another missing path is listed under it;
- shared host paths that are symlinks to paths that do not exist;
- mount points that would have to be created inside a read-only mount;
- settings this host cannot provide: `network.mode: 'isolated'` without
  pasta, `dbus.mode: 'host'` without a `unix:path=` session bus,
  `dbus-daemon` missing for the private bus, and `xwayland-satellite` or
  `Xwayland` missing for `xwayland`;
- Landlock older than required, and cgroup v2 mounted without `nsdelegate`.

Issues that would not stop the sandbox, such as the host-network fallback,
are listed with the report; on a launch that succeeds they are warnings. The
supervisor repeats the check when it starts, and a live config change that
fails it is not applied.

## Running sandboxes

`list` shows every configured sandbox: running instances and tasks, the
network it gets on this host, its apps, and its tasks; a config that does
not load shows its error on one line.

`config` prints a sandbox's config as JSON, evaluated and with every
default filled in, as psbox uses it; `--task` prints one of its tasks'.

`status` shows how long the sandbox has run, its init pid, process and
client counts, network, and X display, then the processes started for
clients, live mounts, and published devices.

`mount` attaches a host path into a running sandbox (at the same path, or
`--at`), read-write or `--ro`; `umount` detaches it. Relative paths are
made absolute against the current directory, and `~` against the home. File-chooser mounts and
config changes use the same mechanism.

`stop` stops a sandbox and returns once systemd sees the service gone.
`stop --all` stops every running sandbox (instances, tasks, and unnamed
ones) through systemd, so it also stops sandboxes still running an older
psbox. It also removes socket units left by unnamed sandboxes whose client
was killed before connecting. `status`, `mount`, `umount`, and `stop` never start a sandbox.

## Setup

`install` writes desktop and autostart entries (see
[configuration.md](configuration.md#desktop-entries)) and reports what it
added, updated, and removed, by sandbox; `-v` also lists unchanged entries
and `--dry-run` writes nothing. It only changes files: it writes and
removes its entries in the directories `XDG_DATA_HOME` and
`XDG_CONFIG_HOME` name, and copies host entries from those and
`XDG_DATA_DIRS` and `XDG_CONFIG_DIRS`, all from its own environment. The
host portal and the supervisors use the systemd user manager's, so the
two must agree for the portal to find the entries. It warns about entries it
cannot build, files another sandbox whose name sorts first claims, and
default apps with no host entry to inherit. Without a `sandboxes`
directory there are no sandboxes, so `install` removes every entry it wrote;
a `sandboxes` path that cannot be read or is not a directory is an error,
and nothing changes.

`devices` lists the host devices that the `gpu`, `camera`, `gamepad`,
`security-key`, and `alsa` classes match; `--all` lists every device, with
the IDs `devices.usb` takes.

`check` lists Landlock, the kernel version, user namespaces (by creating
one, with a mount namespace inside, in a short-lived child process), cgroup
v2 with `nsdelegate`, the units, and `dbus-daemon` as required (`✗`, exit status 1 when one
is missing), and
`xwayland-satellite`, `Xwayland`, and `pasta` as optional (`!`). `check <sandbox>` runs
the [startup check](#the-startup-check) for the sandbox and each of its
tasks without starting anything (exit status 1 when one cannot start);
tasks with the same problems as the sandbox are named once.

`completions bash|zsh` prints the completion script (see
[install.md](install.md#shell-completions)).

## Output

Output is meant for people: borderless tables and lists on stdout, and `✓`
confirmations, `✗` errors, and `!` warnings on stderr. A `→`
line follows only when the fix is not obvious from the message. Problems
that come in groups (the startup check, config errors, `install` warnings)
are shown as sections with a table each. Clap's own usage errors keep
clap's format.

Colors follow the terminal and `NO_COLOR`; `--color always|never`
overrides. On a terminal, tables are fitted to the window (`COLUMNS`
overrides its width) by shortening one column; piped output is never
shortened. Scripts should use `--json`, which `list`, `status`, and
`devices` take.

A sandbox whose replies do not parse is reported as running an older
version of psbox; `psbox stop` restarts it on the current one.

## Unnamed sandboxes

`psbox sandbox` runs a command in a new sandbox that has no config file and
ends when the command exits. Stdio is the caller's terminal and the exit
status passes through. Its config is built from the options, in this order,
each added on top of the last; the home is temporary unless set:

| Option | Meaning |
| --- | --- |
| `--profile NAME` | `$XDG_CONFIG_HOME/psbox/profiles/NAME.libsonnet`, or the built-in profile `NAME`; repeatable |
| `--file FILE` | A Jsonnet config file (its `app` and `task` sections are ignored) |
| `-c`, `--config KEY=VALUE` | Sets a config key; repeatable (see below) |
| `--bind SRC[:DST]` | Share `SRC` read-write at the same path or at `DST`; repeatable |
| `--ro-bind SRC[:DST]` | The same, read-only |
| `--app-id ID` | The [portal identity](configuration.md#portal-identity) of the sandbox's processes; the host portal must accept it, no other sandbox's installed entry may have it, and it needs the private bus |

`KEY` is a dotted path, such as `xwayland.scale` or `environment.EDITOR`; a
name containing a dot is quoted: `-c 'mount."a.b".source=/srv'`. `VALUE` is
read as the key expects: `true` or `false` for a flag, a whole number for
`sandbox.idle-timeout`, `null` to remove (a section, a mount, a variable),
and text for anything else. A value that starts with `"`, `{`, or `[` is
JSON: `-c 'devices.usb=["0483:5740"]'`, `-c 'environment.X="null"'`. A
setting merges into its key, and recreates a section a profile removed.
Paths in `home.path`, `mount.<name>.source`, and `mount.<name>.destination`
(and in `--bind`) may be relative to the current directory or start with
`~`. `app` and `task` keys are refused, as are mount names starting with
`bind-` or `ro-bind-`, which belong to `--bind` and `--ro-bind`.

```sh
psbox sandbox -c network=null -c display=null --bind . -- ./some-tool
psbox sandbox -c home.mode=path -c home.path=~/work/client --bind ~/Downloads -c dbus.mode=host -- bash
psbox sandbox --profile browser --app-id org.mozilla.firefox -- firefox
```

Each run is its own supervisor under a random `sandbox.<16 hex digits>` key.
The client evaluates the settings, writes the result to
`$XDG_RUNTIME_DIR/psbox/<key>.json`, and starts `psbox@<key>.socket`; the
supervisor reads and removes the file. The socket unit is stopped when the
command has exited, by the client and by the supervisor as it exits. A
supervisor whose client leaves without asking for a command stops after 10
seconds. Unnamed sandboxes are not listed and cannot be addressed by
`status`, `mount`, or `stop` (but `stop --all` stops them).
