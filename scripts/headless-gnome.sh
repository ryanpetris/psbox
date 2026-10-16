#!/bin/sh
# Runs GNOME Shell headless for GUI tests, isolated from any real session:
# its own Wayland socket, a private session bus, in-memory GSettings, and
# throwaway XDG directories. Prints the variables to use, then waits.
#
#   scripts/headless-gnome.sh [socket-name] [WxH]
#
# Stop it with Ctrl-C (or by killing the printed gnome-shell pid).
set -eu

name=${1:-psbox-test-0}
size=${2:-1280x800}
: "${XDG_RUNTIME_DIR:=/run/user/$(id -u)}"
export XDG_RUNTIME_DIR
state=$(mktemp -d /var/tmp/psbox-gnome.XXXXXX)
mkdir -p "$state/config" "$state/data" "$state/cache" "$state/state"
trap 'rm -rf "$state"' EXIT

unset DBUS_SESSION_BUS_ADDRESS
dbus-run-session -- sh -c '
    echo "$DBUS_SESSION_BUS_ADDRESS" > "$1/bus"
    exec env GSETTINGS_BACKEND=memory XDG_SESSION_TYPE=wayland \
        XDG_CONFIG_HOME="$1/config" XDG_DATA_HOME="$1/data" \
        XDG_CACHE_HOME="$1/cache" XDG_STATE_HOME="$1/state" \
        gnome-shell --headless --wayland --no-x11 \
            --virtual-monitor "$3" --wayland-display "$2"
' sh "$state" "$name" "$size" > "$state/shell.log" 2>&1 &

for _ in $(seq 1 100); do
    [ -S "$XDG_RUNTIME_DIR/$name" ] && [ -s "$state/bus" ] && break
    sleep 0.1
done
echo "WAYLAND_DISPLAY=$name"
echo "bus file: $state/bus"
echo "log: $state/shell.log"
wait
