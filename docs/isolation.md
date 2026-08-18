# Isolation

Isolation is how a launch enters a sandbox. It is not the CLI verb
`psbox launch`.

| Value | Default? | Host process | In-sandbox PID 2 | Joins a running sandbox | Host URL forward |
| --- | --- | --- | --- | --- | --- |
| `instance` | yes | `psboxd` | `psboxa` | yes | default on |
| `instance:<name>` | | `psboxd` | `psboxa` | that name | default on |
| `oneshot` | | `bwrap` only | the workload | no | no |

Set it in YAML (`options.isolation`) or override on the CLI
(`--isolation` / `--instance`).

## Instance (default)

Use this when:

- a second launch should join the same windows (browsers, IDEs)
- a forking launcher must outlive the CLI (`code`, similar tools)
- an app should open `http`/`https` on the host (`options.host-urls`,
  default on; `metadata.type: browser` turns it off)

`psbox` starts `psboxd@<escaped>.socket` if needed. It does not start
the service unit; socket activation does. It then connects with
`SOCK_SEQPACKET` and sends one spawn with stdio fds. See
[instances.md](instances.md).

The implicit instance name is `default`. `--instance work` is
`instance:work`. Identity is `<sandbox>/<instance>`.

## Oneshot

Use this when you want a single `bwrap` that dies with the process:
one-off commands, tests, or apps that must not share a long-lived
namespace.

```yaml
options:
  isolation: oneshot
```

```sh
psbox launch --isolation oneshot scratch
```

Oneshot `exec`s `/usr/bin/bwrap` with the composed flags and the
workload (plus `dbus-launch` when `dbus: private`). There is no agent,
no unit, and no `open_uri` path. GIO and `xdg-open` stay inside that
sandbox.

## Print

`--print` and `--print-bwrap` format the oneshot-shaped command even
when the application is instance isolation. They never start `psboxd`.
