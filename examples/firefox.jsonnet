// A browser. The built-in profile adds the camera, security keys, a shared
// ~/Downloads, and keeps web links in the browser; the host network lets it
// cast and reach services on localhost. The default app, `firefox`, runs
// `firefox` and inherits the host's firefox.desktop.
local psbox = import 'psbox';

psbox.profiles.browser + {
  network: { mode: 'host' },
}
