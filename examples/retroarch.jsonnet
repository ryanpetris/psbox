// RetroArch from its AppImage, kept in ~/Sandbox/retroarch.bin and overlaid
// at ~/.bin: the app can write there, but its changes are discarded. The
// update task mounts it read-write instead. Games are shared read-only, and
// saves stay in the private home.
local psbox = import 'psbox';
local bin = psbox.path([psbox.home, '.bin']);

{
  devices+: { gamepad: true },
  // The sandbox has no FUSE, so the AppImage unpacks itself to run.
  environment+: { APPIMAGE_EXTRACT_AND_RUN: '1' },
  mount+: {
    games: psbox.mount.readOnly(psbox.path([psbox.home, 'Games']), create=true),
    bin: psbox.mount.overlay(psbox.storageFor('bin'), destination=bin, create=true),
  },
  app+: {
    retroarch: {
      run: [psbox.path([bin, 'RetroArch.AppImage'])],
      keys: {
        Name: 'RetroArch',
        Icon: psbox.path([psbox.storageFor('bin'), 'retroarch.png']),
        StartupWMClass: 'retroarch',
        Categories: ['Game', 'Emulator'],
      },
    },
  },
  task+: {
    update: {
      script: |||
        set -euo pipefail
        cd ~/.bin
        echo "download the new RetroArch.AppImage and retroarch.png here" >&2
      |||,
      overrides: {
        home: { mode: 'temporary' },
        display: null,
        devices: psbox.devices.none,
        mount+: { bin+: { mode: 'read-write' } },
      },
    },
  },
}
