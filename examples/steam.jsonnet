// The client is X11-only, and Remote Play needs the local network. The
// default app, `steam`, inherits the host's steam.desktop.
{
  xwayland: {},
  network: { mode: 'host' },
  home+: { cache: 'persistent' },
  devices+: { gamepad: true },
}
