#!/usr/bin/python3 -I
"""Screenshots and window lists from a headless GNOME Shell test instance.

GNOME Shell only answers these calls for callers that own an allowlisted
name, so this claims one. Only use it against the private bus printed by
headless-gnome.sh, never a real session bus.

    gnome-shell-probe.py BUS_FILE shot OUTPUT.png
    gnome-shell-probe.py BUS_FILE windows
"""

import sys

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

ALLOWLISTED = ("org.gnome.SettingsDaemon.MediaKeys", "org.freedesktop.impl.portal.desktop.gnome")


def connect(bus_file):
    address = open(bus_file).read().strip()
    flags = Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION
    bus = Gio.DBusConnection.new_for_address_sync(address, flags, None, None)
    for name in ALLOWLISTED:
        bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
                      GLib.Variant("(su)", (name, 4)), None, 0, -1, None)
    return bus


def main():
    bus = connect(sys.argv[1])
    if sys.argv[2] == "shot":
        r = bus.call_sync("org.gnome.Shell.Screenshot", "/org/gnome/Shell/Screenshot", "org.gnome.Shell.Screenshot",
                          "Screenshot", GLib.Variant("(bbs)", (False, False, sys.argv[3])), None, 0, 10000, None)
        print(r.unpack()[1])
    elif sys.argv[2] == "windows":
        r = bus.call_sync("org.gnome.Shell.Introspect", "/org/gnome/Shell/Introspect", "org.gnome.Shell.Introspect",
                          "GetWindows", None, None, 0, 10000, None)
        keys = ("title", "app-id", "wm-class", "sandboxed-app-id")
        for wid, props in r.unpack()[0].items():
            print(wid, {k: v for k, v in props.items() if k in keys})
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
