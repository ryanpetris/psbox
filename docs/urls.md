# Host URLs

Instance isolation can open `http` and `https` on the host. Other
schemes stay in the sandbox. Oneshot isolation has no forwarder.

This is how Sandbox B (Warpinator, an editor, …) opens a link in
Sandbox A (the host default browser, which may itself be `psbox`).

## Paths that work

1. **`xdg-open` on `PATH`.** Compose creates
   `$XDG_RUNTIME_DIR/psbox/bin/xdg-open` as a symlink to `psboxa` and
   leaves `/usr/bin/xdg-open` alone. `psboxa` prepends that directory
   to the workload `PATH`.
2. **GIO / GTK (`gtk_show_uri`, `GtkLinkButton`).** On each spawn,
   `psboxa` writes `$XDG_DATA_HOME/applications/psbox-open-url.desktop`
   (`Exec=<psboxa> xdg-open %u`) and sets
   `x-scheme-handler/http` and `x-scheme-handler/https` in
   `$XDG_CONFIG_HOME/mimeapps.list`. Existing keys and sections are
   kept. The desktop file is overwritten. The mimeapps file is rewritten
   only when those two defaults need to change.

Both end at `psboxa` acting as the helper (`argv[0]` is `xdg-open`, or
`psboxa xdg-open …`). The helper talks to
`$XDG_RUNTIME_DIR/psbox-xdg-open.sock` (mode `0600`). The long-lived
agent sends `open_uri` to `psboxd`. `psboxd` allowlists `http`/`https`
and runs host `xdg-open`. Up to four opens run concurrently, each with a
5-second deadline. Excess requests receive an error. Slow opens do not hold
up spawn, exit, or liveness replies. Sandbox shutdown cancels and joins
outstanding open operations.

Host `xdg-open` uses host desktop files. After `psbox objects install`,
the default browser's `Exec=` is `psbox launch <browser> …`, so the URL joins
that browser's instance.

## What stays inside

- `file:`, `mailto:`, and any non-`http(s)` URI
- absolute `/usr/bin/xdg-open` (the real helper)
- oneshot sandboxes
- a GIO open that still has some other default in `mimeapps.list`
  because the file was not writable

The helper execs `/usr/bin/xdg-open` by absolute path for anything it
does not forward, so it cannot loop back into itself.

## Portals

psbox does not claim `org.freedesktop.portal.Desktop` and does not set
`GIO_USE_PORTALS`. A private session bus is only a session bus. When
the sandbox is on the host bus (`dbus: true`), portal Settings and
file choosers are the host portal.

## Allowlist

The agent and the daemon both reject non-`http(s)` URIs. The daemon
check is the one that matters: the host never sees `file:` through
this path.

## Setup requirements

The application must use instance isolation (the default) and leave
`options.host-urls` on (the default). `host-urls: false` disables this
path: no `PATH` shim, no GIO defaults, no xdg-open listener. On the
next spawn it deletes `psbox-open-url.desktop` and drops the two
http/https default keys only if they still point at that desktop file.

`psboxa` must be the agent inside that instance. Writing
`mimeapps.list` happens in the sandbox at spawn time, using the
workload `HOME` / `XDG_CONFIG_HOME` / `XDG_DATA_HOME`. The host does
not write those files.

Browsers should set `metadata.type: browser` (or `host-urls: false`).
They are the destination, and the GIO default would make them report
that they are not the default browser.
