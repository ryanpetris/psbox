# Petris Sandbox

Give every Linux application its own home. Keep the host desktop.

Petris Sandbox (`psbox`) launches apps inside a Bubblewrap sandbox with a
private home directory. The app still sees your display, speakers, and
system files. It does not see your real `~/`. Profiles, caches, and
other app data stay in that sandbox.

## Quick start

You need Linux, [`bubblewrap`](https://github.com/containers/bubblewrap),
and [`xdg-utils`](https://www.freedesktop.org/wiki/Software/xdg-utils/).
`dbus` is optional and used for a private session bus. A systemd user
session is required.

### 1. Install

From a [release archive](docs/install.md):

```sh
sudo install -m755 psbox psboxd psboxa /usr/bin/
sudo install -m644 systemd/psboxd@.socket systemd/psboxd@.service /usr/lib/systemd/user/
systemctl --user daemon-reload
```

Or install the package your distribution provides, if it has one.

### 2. Define an application

Create `~/.config/psbox/objects/firefox.yaml`:

```yaml
version: v1
kind: Application
metadata:
  type: browser
---
version: v1
kind: FreedesktopEntry
```

The private home defaults to `~/Sandbox/firefox`. Extra binds are
optional. The desktop entry is taken from the system
`firefox.desktop` and rewritten so the menu runs `psbox launch firefox`.

### 3. Install launchers and run

```sh
psbox objects install
psbox launch firefox
```

The desktop entry now starts the sandboxed Firefox.

## Next

The [documentation](docs/index.md) covers installation, object YAML,
the CLI, isolation modes, the instance daemon, and how URLs leave a
sandbox.

## License

MIT. See [LICENSE](LICENSE).
