# Install

## Requirements

| What | Needed for |
| --- | --- |
| Linux 7.2 or newer | psbox uses the newest kernel interfaces without fallbacks |
| Landlock ABI 6 or newer | The abstract-socket and signal scopes every sandbox runs in |
| Unprivileged user namespaces | Each supervisor creates its own user namespace (an AppArmor restriction on them must allow psbox) |
| cgroup v2 | The `supervisor/`, `infra/`, and `workload/` cgroups inside each unit |
| A systemd user session | Socket-activated supervisor units |
| `dbus-daemon` | The private session bus (`dbus = "portal"`, the default) |
| `pasta` (package `passt`), optional | Isolated networking; without it, sandboxes that do not set `network` use the host network |
| `xwayland-satellite`, optional | X11 apps (`xwayland = true`) |

`psbox check` reports each of these.

## From a package

Each release has an Arch package (`psbox-<version>-<rel>-x86_64.pkg.tar.zst`)
and Debian packages for amd64 and arm64 (`psbox_<version>-<rel>_<arch>.deb`,
built on Debian 13). They install the binary, the user units, and the bash
and zsh completions, and depend on `dbus` and `systemd`; `passt` and
`xwayland-satellite` are optional:

```sh
sudo pacman -U psbox-<version>-1-x86_64.pkg.tar.zst      # Arch
sudo apt install ./psbox_<version>-1_amd64.deb            # Debian, Ubuntu
systemctl --user daemon-reload
psbox check
```

## From a release archive

Each archive (`psbox_<version>_linux_<arch>.tar.gz`, see
[release.md](release.md)) holds the static `psbox` binary, the two user
units, the license, and the README:

```sh
tar xzf psbox_<version>_linux_amd64.tar.gz
cd psbox_<version>_linux_amd64
sudo install -m755 psbox /usr/bin/
sudo install -m644 units/psbox@.socket units/psbox@.service /usr/lib/systemd/user/
systemctl --user daemon-reload
psbox check
```

## From source

With a stable Rust toolchain:

```sh
cargo build --release
sudo make install                     # or: make install DESTDIR=<package root>
systemctl --user daemon-reload
```

`make install` installs the release binary as `$(PREFIX)/bin/psbox`
(`PREFIX` defaults to `/usr`) and the units in
`$(PREFIX)/lib/systemd/user`, pointing at it.

For development, `make dev/install` builds the debug binary and installs
the units into `$XDG_CONFIG_HOME/systemd/user` pointing at it, which
overrides packaged units. See [development.md](development.md).

## First sandbox

Sandbox configs live in `$XDG_CONFIG_HOME/psbox/sandboxes/<name>.toml`
(see [configuration.md](configuration.md)). [examples/](../examples/) has
configs for several applications. Then:

```sh
psbox list                 # configured sandboxes
psbox check <name>         # whether it can start, without starting it
psbox install              # desktop and autostart entries
psbox run <name>           # start, or join, the sandbox
```

## Shell completions

Completions are dynamic: the shell calls back into psbox, which completes
commands, flags, flag values, and sandbox names. Load them at shell startup
rather than saving the script, so they always match the binary:

```sh
echo 'source <(psbox completions bash)' >> ~/.bashrc
echo 'source <(psbox completions zsh)' >> ~/.zshrc
```

## Uninstall

`psbox stop --all`, remove the binary and the two units, and run
`systemctl --user daemon-reload`. `psbox install` entries are files in
`$XDG_DATA_HOME/applications` and `$XDG_CONFIG_HOME/autostart` that start
with `# @generator psbox`; sandbox homes are in `$PSBOX_ROOT` (default
`~/Sandbox`).
