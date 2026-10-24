// The client is X11-only and 32-bit, and Remote Play needs the local
// network. The default app, `steam`, inherits the host's steam.desktop.
{
  sandbox+: { '32-bit': true },
  xwayland: {},
  network: { mode: 'host' },
  home+: { cache: 'persistent' },
  devices+: { gamepad: true },
}
