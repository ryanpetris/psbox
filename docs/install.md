# Install

psbox is Linux-only. It needs:

- [`bwrap`](https://github.com/containers/bubblewrap) at `/usr/bin/bwrap`
- [`xdg-open`](https://www.freedesktop.org/wiki/Software/xdg-utils/) on
  the host (package `xdg-utils` on most distributions)
- a systemd **user** session, for instance isolation
- `dbus-daemon`, if any application uses `options.dbus: private`

## From a release archive

Download the `amd64` or `arm64` tarball from the GitHub Release. Each
archive contains `psbox`, `psboxd`, `psboxa`, the user units, and shell
completions. See [release.md](release.md).

```sh
sudo install -m755 psbox psboxd psboxa /usr/bin/
sudo install -m644 systemd/psboxd@.socket systemd/psboxd@.service /usr/lib/systemd/user/
sudo install -m644 completions/psbox.bash /usr/share/bash-completion/completions/psbox
sudo install -m644 completions/_psbox /usr/share/zsh/site-functions/_psbox
systemctl --user daemon-reload
```

The units are templates. Do not enable `psboxd@.service` globally.
`psbox` starts `psboxd@<identity>.socket` when it launches an instance.

## From source

Use the Go version in `go.mod`. From the repository root:

```sh
make go/install
sudo install -m644 systemd/psboxd@.socket systemd/psboxd@.service /usr/lib/systemd/user/
systemctl --user daemon-reload
```

`make go/install` writes the three binaries to `GOBIN` (or `$(go env GOPATH)/bin`).
Put that directory on `PATH`, or copy the binaries to `/usr/bin` so
`psboxd@.service` can exec `/usr/bin/psboxd`.

Set `PSBOX_AGENT_BIN` if `psboxa` is not at `/usr/bin/psboxa`.

## First objects

Default objects path: `$XDG_CONFIG_HOME/psbox/objects`
(`~/.config/psbox/objects`). Create it and add at least one
`Application`. A desktop entry is optional until you want a menu item.

```sh
mkdir -p ~/.config/psbox/objects
```

Example `~/.config/psbox/objects/firefox.yaml`:

```yaml
version: v1
kind: Application
metadata:
  type: browser
---
version: v1
kind: FreedesktopEntry
```

Then:

```sh
psbox objects install
psbox launch firefox
```

`psbox objects install` rewrites `~/.local/share/applications/firefox.desktop`
so the desktop runs `psbox launch firefox`. It looks up the system
`firefox.desktop` when `spec.value` is omitted.

Private homes default to `~/Sandbox/<application>`. Override the root
with `PSBOX_ROOT` or a collection `Options` object. See
[configuration.md](configuration.md).

## Check the install

```sh
psbox --version
psbox objects list applications
psbox launch --print firefox
```

`--print` writes a `psbox sandbox …` command to stdout. It does not
start `psboxd`. `psbox launch --print-bwrap firefox` writes the `bwrap` argv.

If instance launch fails with a socket error, confirm the user manager
is running (`systemctl --user status`) and that the units are in
`/usr/lib/systemd/user/`.

## Distribution packages

Distro recipes install the same three binaries and the in-tree
`systemd/` templates to `/usr/lib/systemd/user/`. Do not keep a second
copy of the units in this tree.
