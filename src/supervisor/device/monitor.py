"""Read psbox device publications with the host's installed libudev."""

import ctypes as C
import errno
import os
import select
import socket
import stat
import sys
from pathlib import Path

udev = C.CDLL("libudev.so.1", use_errno=True)
ptr, string, integer = C.c_void_p, C.c_char_p, C.c_int


def bind(name, result, *args):
    fn = getattr(udev, "udev_" + name)
    fn.restype, fn.argtypes = result, args
    return fn


new = bind("new", ptr)
new_monitor = bind("monitor_new_from_netlink", ptr, ptr, string)
enable = bind("monitor_enable_receiving", integer, ptr)
fd = bind("monitor_get_fd", integer, ptr)
subsystem_filter = bind("monitor_filter_add_match_subsystem_devtype", integer, ptr, string, string)
tag_filter = bind("monitor_filter_add_match_tag", integer, ptr, string)
receive = bind("monitor_receive_device", ptr, ptr)
unref = bind("device_unref", ptr, ptr)
property_value = bind("device_get_property_value", string, ptr, string)
subsystem = bind("device_get_subsystem", string, ptr)
devnode = bind("device_get_devnode", string, ptr)
devpath = bind("device_get_devpath", string, ptr)
has_tag = bind("device_has_tag", integer, ptr, string)
has_current_tag = bind("device_has_current_tag", integer, ptr, string)
seqnum = bind("device_get_seqnum", C.c_ulonglong, ptr)
action = bind("device_get_action", string, ptr)
context = new()
assert context


def monitor(sub=None, tag=None):
    mon = new_monitor(context, b"udev")
    assert mon
    if sub:
        assert subsystem_filter(mon, sub, None) == 0
    if tag:
        assert tag_filter(mon, tag) == 0
    assert enable(mon) >= 0
    return mon


control = socket.socket(fileno=0)
control.settimeout(6)
caps = [line.split()[1] for line in Path("/proc/self/status").read_text().splitlines()
        if line.startswith(("CapEff:", "CapInh:", "CapPrm:", "CapAmb:", "CapBnd:"))]
assert len(caps) == 5 and all(int(cap, 16) == 0 for cap in caps), caps
sender_uid = int.from_bytes(control.recv(4), sys.byteorder)
assert os.getuid() == os.geteuid() == sender_uid != 0

with socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, 15) as sock:
    try:
        sock.sendto(b"fixture", (0, 2))
    except OSError as e:
        assert e.errno == errno.EPERM, e
    else:
        raise AssertionError("unprivileged receiver sent multicast")

# On a private /dev tmpfs libudev checks for a running udev daemon.
os.rename("/run/udev/control", "/run/udev/masked")
masked = monitor()
os.rename("/run/udev/masked", "/run/udev/control")
wanted = monitor(b"input", b"seat")
quiet = [fd(masked), fd(monitor(b"drm")), fd(monitor(tag=b"missing"))]

node, alias = b"/dev/input/event6", b"/dev/input/by-id/pad"
hidden = "/dev/.psbox/c1-3"


def event(expected, seq):
    assert select.select([fd(wanted)], [], [], 5)[0], f"no libudev {expected!r}"
    device = receive(wanted)
    assert device, f"libudev rejected {expected!r} (errno {C.get_errno()})"
    assert action(device) == expected and seqnum(device) == seq
    assert subsystem(device) == b"input" and devnode(device) == node
    assert devpath(device) == b"/devices/virtual/input/event6"
    for key, value in [(b"ID_INPUT_JOYSTICK", b"1"), (b"WL_SEAT", b"game"),
                       (b"USEC_INITIALIZED", b"1234"), (b"DEVLINKS", alias)]:
        assert property_value(device, key) == value, (expected, key, property_value(device, key))
    assert has_tag(device, b"seat") and has_current_tag(device, b"seat")
    assert has_tag(device, b"old") and not has_current_tag(device, b"old")
    assert not select.select(quiet, [], [], 0)[0], "unexpected monitor received the event"
    unref(device)


control.send(b"listening")
event(b"add", 2)
for path in (node, alias):
    with os.fdopen(os.open(path, os.O_RDWR | os.O_NONBLOCK), "r+b", buffering=0) as dev:
        info = os.fstat(dev.fileno())
        assert stat.S_ISCHR(info.st_mode) and info.st_rdev == os.makedev(1, 3)
        assert dev.read(1) == b""
print(f"libudev add seq=2: uid {sender_uid} opened event6 and its by-id alias", flush=True)
control.send(b"opened")

assert control.recv(64) == b"check cleanup"
assert stat.S_ISCHR(os.stat("/dev/held/c1-3").st_mode)
assert not select.select([fd(wanted), *quiet], [], [], 0)[0], "remove arrived before detach"
control.send(b"still listening")
event(b"remove", 3)
assert all(not os.path.lexists(path) for path in (node, alias, hidden))
print("libudev remove seq=3: node, alias, and hidden mount point are gone", flush=True)
