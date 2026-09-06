# CLI

The `psbox` binary is the host CLI. Incidental status and warnings go
to stderr. stdout is for `--print` / `--print-bwrap`, foreground
application output, and scripting commands (`objects list`,
`objects render`). Log lines are the message only. Set
`PSBOX_LOG=structured` for slog text.

## `psbox launch`

Launch is a required subcommand. `psbox firefox` is not a launch.

```sh
psbox launch firefox
```

```text
psbox launch [flags] <application> [-- args]
```

| Flag | Meaning |
| --- | --- |
| `--objects PATH` | Objects directory |
| `--print` | Print the oneshot `psbox sandbox …` command and exit |
| `--print-bwrap` | Print `bwrap …` and exit |
| `--isolation MODE` | `oneshot`, `instance`, or `instance:name` |
| `--instance NAME` | Join or create that instance name |
| `--command` | Treat the argument list as the full command |

`--isolation` and `--instance` are mutually exclusive.
`--print` and `--print-bwrap` are mutually exclusive.

`--print` and `--print-bwrap` always format the oneshot compose
(workload after `--`, `dbus-launch` when `dbus: private`). They never
print `psboxa`, unit names, or `systemctl`.

Extra arguments after the application name are passed to the workload.
They replace `default-args`. `--command` replaces `exec` as well:

```sh
psbox launch firefox https://example.com
psbox launch firefox --command -- env MOZ_LOG=1 firefox
```

Flags may appear after the application name. `--` still ends flag
parsing for the workload.

`--objects` with instance isolation is an error when `PATH` is not the
process default objects directory.

The process exit status is the workload exit status.

## `psbox instance`

List and stop instantiated `psboxd@` user units. The list comes from
the systemd user manager, not from object YAML.

```text
psbox instance list [-q]
psbox instance stop <name>
psbox instance stop --all
```

`list` writes a table to stdout with `IDENTITY`, `SERVICE`, `SOCKET`,
`PID` (`psboxd` `MainPID`), `SINCE` (service start), `RESTARTS`, and
`ACCEPTED` (socket accepts). Those extra columns come from systemd
unit properties. `-q` prints identities only, one per line, for
scripts. The default instance name is omitted: `firefox/default`
is shown as `firefox`. Other names stay `firefox/work`. Completion
uses the same form.

`stop` stops the service and then the socket. `<name>` is a listed
identity (`firefox` or `firefox/work`), the full `sandbox/default`
form, or a listed escaped `%i`. Unknown names are an error.

`stop --all` stops every listed identity. It is not an error if none
exist. List and stop time out each user-manager D-Bus call and retry
a timed-out or disconnected call up to three times.

## `psbox objects`

```text
psbox objects [--objects PATH] install
psbox objects [--objects PATH] cleanup
psbox objects [--objects PATH] list {applications|desktop|autostart} [-q]
psbox objects [--objects PATH] render <application>
```

| Command | stdout | Notes |
| --- | --- | --- |
| `install` | (none) | Writes desktop files; progress on stderr |
| `cleanup` | (none) | Deletes `# @generator psbox` files |
| `list applications` | one name per line | Omits broken referrers |
| `list desktop` | `APPLICATION` / `ENTRY` table unless `-q` | |
| `list autostart` | `APPLICATION` / `ENTRY` table unless `-q` | |
| `render <application>` | rewritten desktop and autostart text | Does not write files |

`install` skips entries whose template file is missing and lists those
paths on stderr through the CLI logger (`PSBOX_LOG=structured` applies).
Other configured entries are still installed. Other rendering failures return
the actual error and stop installation before orphan cleanup, preserving
existing launchers that could not be rendered. Install and cleanup progress
also use the CLI logger.

Failures writing required print, list, or render output fail the command,
including failures flushing a table.

## `psbox sandbox`

Low-level compose. Requires a home choice and a command after `--`.

```text
psbox sandbox [flags] -- <command>
```

| Flag | Meaning |
| --- | --- |
| `--home DIR` | Bind this directory as `$HOME` |
| `--home-tmpfs` | Empty tmpfs home |
| `--no-home` | No home bind |
| `--no-audio` | No audio devices or sockets |
| `--no-video` | No video devices |
| `--no-dbus` | No system or session bus |
| `--no-session-dbus` | System bus only |
| `--no-fontconfig` | No fontconfig cache |
| `--no-udev` | No `/run/udev` |
| `--no-evdev` | No `/dev/input` or hidraw |
| `--no-display` | No Wayland/X11/DRI |
| `--cache MODE` | `tmpfs` (default) or `home` |
| `--downloads` | Bind host `Downloads` into `$HOME/Downloads` |
| `--print` | Print the `bwrap` argv |

`--home`, `--home-tmpfs`, and `--no-home` are mutually exclusive.
`--no-dbus` and `--no-session-dbus` are mutually exclusive.

`psbox launch` must not exec `psbox sandbox`. Both call the same
in-process compose.

## Completions

```sh
psbox completion bash
psbox completion zsh
```

Release archives include generated files. Application-name completion
runs `psbox objects list applications --quiet`.
