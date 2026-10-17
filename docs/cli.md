# CLI

```sh
psbox run <sandbox> [-i INSTANCE] [--command NAME] [--exec] [--create-dirs] [-- ARGS...]
psbox task <sandbox> <task> [--create-dirs] [-- ARGS...]
psbox sandbox (--home DIR | --home-tmpfs | --no-home) [OPTIONS] [--create-dirs] -- COMMAND...
psbox list [--json]                     # sandboxes, what runs, their network, commands, and tasks
psbox status <sandbox> [-i INSTANCE] [--json]   # processes, mounts, devices
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
socket unit) and runs a command in it; the command's stdin, stdout, and
stderr are the caller's, signals are forwarded, and its exit status is
returned. `--command` picks a `[command.<name>]`; without it, the command
named like the sandbox, or the only one, runs. Arguments after `--` are
appended to the command's `exec`. `--exec` treats them as the whole command
line instead (the command still supplies its environment and app id). `-i`
starts or joins a named instance.

Commands start in the caller's working directory when the sandbox has it
(as with `rw = ["~/Projects/"]` and a caller in `~/Projects/x`), else in the
home, else in `/`.

`psbox task <sandbox> <task>` runs a task in its own supervisor and waits
for it. Afterwards, running instances of the sandbox replace their overlays
over the task's `writable` directories, so they see what it changed (see
[configuration.md](configuration.md#instances-and-tasks)); an instance that
cannot is named in a warning, and keeps the old files until it restarts.
`--create-dirs` on `run`, `task`, and `sandbox` also creates missing
directories marked with a trailing `/` outside the sandbox root, and
confirms each one (see [configuration.md](configuration.md#paths-and-tokens)).
Paths without the `/` are never created; the startup check says so when
`--create-dirs` was given.

`run`, `task`, and `sandbox` print nothing of their own (stdio belongs to
the command), only warnings: a sandbox on the host-network fallback, or one
still running an older config.

## The startup check

Before a sandbox starts, `run`, `task`, and `sandbox` check everything known
to stop it, and list every problem at once, by section, instead of failing
on the first:

- every config validation error, by config entry (a TOML syntax error shows
  its line);
- shared host paths that do not exist, named by their `rw`, `ro`, or
  `[mount.<name>]` entry, unless the mount is `optional` or psbox creates
  the path; a missing path inside another missing path is listed under it;
- shared host paths that are symlinks to paths that do not exist;
- mount points that would have to be created inside a read-only mount;
- settings this host cannot provide: `network = "isolated"` without pasta,
  `dbus = "host"` without a `unix:path=` session bus, and `dbus-daemon`
  missing for the private bus;
- Landlock older than required.

Issues that would not stop the sandbox, such as the host-network fallback,
are listed with the report; on a launch that succeeds they are warnings. The
supervisor repeats the check when it starts, and a live config change that
fails it is not applied.

## Running sandboxes

`list` shows every configured sandbox: running instances and tasks, the
network it gets on this host, its commands, and its tasks; a config that
does not load shows its error on one line.

`status` shows how long the sandbox has run, its init pid, process and
client counts, network, and X display, then the processes started for
clients, live mounts, and published devices.

`mount` attaches a host path into a running sandbox (at the same path, or
`--at`), read-write or `--ro`; `umount` detaches it. File-chooser mounts and
config changes use the same mechanism.

`stop` stops a sandbox and returns once systemd sees the service gone.
`stop --all` stops every running sandbox (instances, tasks, and unnamed
ones) through systemd, so it also stops sandboxes still running an older
psbox. It also removes socket units left by unnamed sandboxes whose client
was killed before connecting. `status`, `mount`, `umount`, and `stop` never start a sandbox.

## Setup

`install` writes desktop and autostart entries (see
[configuration.md](configuration.md#desktop-and-autostart-entries)) and
reports what it added, updated, and removed, by sandbox; `-v` also lists
unchanged entries and `--dry-run` writes nothing. Without a `sandboxes`
directory there are no sandboxes, so `install` removes every entry it wrote;
a `sandboxes` path that cannot be read or is not a directory is an error,
and nothing changes.

`devices` lists the host devices that the `gpu`, `camera`, `gamepad`,
`security-key`, and `alsa` classes match; `--all` lists every device, with
the IDs `usb:<vid>:<pid>` needs.

`check` lists Landlock, the kernel version, user namespaces (by creating
one, with a mount namespace inside, in a short-lived child process), cgroup
v2, the units, and `dbus-daemon` as required (`✗`, exit status 1 when one
is missing), and
`xwayland-satellite` and `pasta` as optional (`!`). `check <sandbox>` runs
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
status passes through. Exactly one home flag is required:

| Flag | Meaning |
| --- | --- |
| `--home DIR` | Bind `DIR` read-write as the home (`~`, `@root`, absolute, or relative to the current directory) |
| `--home-tmpfs` | An empty in-memory home |
| `--no-home` | No home |
| `--cache tmpfs\|home` | As the `cache` key |
| `--devices LIST` | Comma-separated classes replacing `gpu,audio`; `none` for no devices |
| `--dbus portal\|host\|off` | As the `dbus` key |
| `--dbus-talk NAME` | A host bus name the sandbox may call, as the `dbus-talk` key; repeatable |
| `--open-urls SCHEME=host\|sandbox` | Where `http`, `https`, or `mailto` links open, as the `[open-urls]` table; repeatable |
| `--network isolated\|host\|none` | As the `network` key |
| `--no-display` | No Wayland and no Xwayland |
| `--xwayland` | Xwayland for X11 apps, as the `xwayland` key |
| `--bind SRC[:DST]` | Share `SRC` read-write at the same path or at `DST`; repeatable; a trailing `/` marks a directory |
| `--ro-bind SRC[:DST]` | The same, read-only |
| `--env K=V` | An environment variable; repeatable |

```sh
psbox sandbox --home-tmpfs --network none --no-display --bind . -- ./some-tool
psbox sandbox --home ~/work/client --bind ~/Downloads/ --dbus host -- bash
```

Each run is its own supervisor under a random `sandbox.<16 hex digits>` key.
The client writes the settings to `$XDG_RUNTIME_DIR/psbox/<key>.toml` and
starts `psbox@<key>.socket`; the supervisor reads and removes the file. The
socket unit is stopped when the command has exited, by the client and by
the supervisor as it exits. A supervisor whose client leaves without asking
for a command stops after 10 seconds. Unnamed sandboxes are not listed and
cannot be addressed by `status`, `mount`, or `stop` (but `stop --all` stops
them).
