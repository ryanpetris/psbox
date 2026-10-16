# Petris Sandbox

Give every Linux application its own home. Keep the host desktop.

Petris Sandbox (`psbox`) runs applications in sandboxes with a private home
directory. An app still sees your display, speakers, GPU, and system files,
and talks to the desktop through portals. It does not see your real `~/`,
other apps' data, or devices you have not granted it. Profiles, caches, and
other app data stay in the sandbox.

psbox is one binary built directly on Linux namespaces, the new mount
API, Landlock, seccomp, and cgroups. Sandboxes run as systemd user services,
start on demand, accept live mounts and hotplugged devices, and stop when
idle.

## Quick start

You need Linux 7.2 or newer with unprivileged user namespaces and cgroup v2,
a systemd user session, and `dbus-daemon`. `pasta` (from `passt`) gives
sandboxes their own network; `xwayland-satellite` runs X11 apps.

### 1. Install

From a [release archive](docs/install.md):

```sh
sudo install -m755 psbox /usr/bin/
sudo install -m644 units/psbox@.socket units/psbox@.service /usr/lib/systemd/user/
systemctl --user daemon-reload
psbox check
```

### 2. Configure a sandbox

Create `~/.config/psbox/sandboxes/firefox.toml`:

```toml
extends = ["browser"]
network = "host"

[command.firefox]
exec = ["firefox"]
desktop = {}
```

The private home is `~/Sandbox/firefox`. The built-in `browser` profile
adds the camera, security keys, and a shared `~/Downloads`.

### 3. Install launchers and run

```sh
psbox install
psbox run firefox
```

The desktop entry now starts the sandboxed Firefox.

## Next

The [documentation](docs/index.md) covers installation, configuration, the
CLI, what a sandbox can reach, and how psbox works.

## License

MIT. See [LICENSE](LICENSE).
