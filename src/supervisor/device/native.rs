//! Exercises device reconciliation with a native libudev listener in a private network namespace.

use super::*;
use anyhow::ensure;
use std::env;
use std::os::fd::FromRawFd;
use std::os::unix::{fs::symlink, process::CommandExt};
use std::process::{Command, Stdio};

const TEST: &str = "supervisor::device::native::private_libudev";
const KEY: Key = (1, 3); // /dev/null

#[test]
#[ignore = "requires native libudev and unprivileged namespaces (make test/devices)"]
fn private_libudev() -> Result<()> {
    // SAFETY: getuid takes no arguments.
    let uid = unsafe { libc::getuid() };
    ensure!(uid != 0, "run the device fixture as an ordinary user");
    ensure!(
        ns::kernel_version()? >= (6, 5),
        "the device fixture requires Linux 6.5+"
    );
    match env::var("PSBOX_DEVICE_TEST_ROLE").as_deref() {
        Ok("sender") => sender(uid),
        Ok("init") => init(),
        Err(env::VarError::NotPresent) => {
            // PID 1 exiting also kills the listener if an assertion or the timeout fires.
            let status = Command::new("timeout")
                .args([
                    "-k",
                    "2",
                    "30",
                    "unshare",
                    "--map-current-user",
                    "--keep-caps",
                    "--net",
                    "--mount-proc",
                    "--pid",
                    "--fork",
                    "--kill-child",
                    "--propagation",
                    "private",
                ])
                .arg(env::current_exe()?)
                .args(["--exact", TEST, "--ignored", "--nocapture"])
                .env("PSBOX_DEVICE_TEST_ROLE", "sender")
                .status()?;
            ensure!(status.success(), "device fixture exited with {status}");
            Ok(())
        }
        _ => bail!("unknown device fixture role"),
    }
}

fn mount_tmpfs(path: &str) -> Result<()> {
    let tree = mount::tmpfs(0o755, MOUNT_ATTR_NOSUID)?;
    mount::attach(tree.as_fd(), mount::open_dir(Path::new(path))?.as_fd())
        .with_context(|| format!("mount fixture tmpfs at {path}"))
}

fn fixture_host() -> Result<HostPaths> {
    // All three host paths live on a mount private to the fixture sender.
    mount_tmpfs("/dev/shm")?;
    let base = Path::new("/dev/shm");
    let host = HostPaths {
        dev: base.join("dev"),
        udev_data: base.join("data"),
        sys_dev_char: base.join("sys/dev/char"),
    };
    let sys = base.join("sys/devices/virtual/input/event6");
    for dir in [&sys, &host.sys_dev_char, &host.udev_data, &host.dev.join("input/by-id")] {
        fs::create_dir_all(dir)?;
    }
    symlink(&sys, host.sys_dev_char.join("1:3"))?;
    symlink("/sys/class/input", sys.join("subsystem"))?;
    fs::write(sys.join("uevent"), "MAJOR=1\nMINOR=3\nDEVNAME=input/event6\n")?;
    fs::write(
        host.udev_data.join("c1:3"),
        "I:1234\nE:ID_INPUT_JOYSTICK=1\nE:WL_SEAT=game\nE:PAD-PROFILE=usb\nG:old\nG:seat\nQ:seat\nS:input/by-id/pad\n",
    )?;
    let tree = mount::clone_tree(Path::new("/dev/null"), false)?;
    let point = mount::make_point(
        mount::open_dir(&host.dev)?.as_fd(),
        Path::new("input/event6"),
        PointKind::File,
    )?;
    mount::attach(tree.as_fd(), point.as_fd())?;
    symlink("../event6", host.dev.join("input/by-id/pad"))?;
    Ok(host)
}

fn expect(chan: &Seqpacket, expected: &[u8]) -> Result<()> {
    let mut buf = [0; 80];
    let (n, _) = chan.recv(&mut buf)?.context("listener exited")?;
    ensure!(
        &buf[..n] == expected,
        "expected {:?}, received {:?}",
        expected,
        &buf[..n]
    );
    Ok(())
}

fn sender(uid: u32) -> Result<()> {
    ensure!(std::process::id() == 1, "fixture requires a private PID namespace");
    let map = fs::read_to_string("/proc/self/uid_map")?;
    ensure!(map.split_whitespace().collect::<Vec<_>>() == [uid.to_string(), uid.to_string(), "1".into()]);
    let host = fixture_host()?;
    let outside = devices::Watcher::new()?;
    let (chan, peer) = Seqpacket::pair()?;
    // The init receives its seqpacket on descriptor 3 through the same activation
    // convention as a supervisor. stdin carries the listener's acknowledgments.
    let mut child = Command::new("unshare")
        .args([
            "--mount",
            "--net",
            "--propagation",
            "private",
            "sh",
            "-c",
            "exec 3<&0; LISTEN_PID=$$ LISTEN_FDS=1 exec \"$@\"",
            "sh",
        ])
        .arg(env::current_exe()?)
        .args(["--exact", TEST, "--ignored", "--nocapture"])
        .env("PSBOX_DEVICE_TEST_ROLE", "init")
        .stdin(Stdio::from(peer.as_fd().try_clone_to_owned()?))
        .spawn()?;
    drop(peer);
    let ready = root::Ready::receive(&chan, true)?;
    ensure!(
        ready.udev_control && ready.sender.is_some(),
        "missing control path or sender"
    );
    ensure!(fs::read_link("/proc/self/ns/net")? != fs::read_link(format!("/proc/{}/ns/net", child.id()))?);
    // SAFETY: pidfd_open takes a live child PID and returns a new descriptor on success.
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0) };
    let pidfd = cvt_int(pidfd as i32)?;
    // SAFETY: pidfd_open transferred ownership.
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd) };
    let mut events = DeviceEvents::new(ready.sender);
    let dev = devices::read_device_checked(&host, KEY.0, KEY.1)?.context("missing fixture record")?;
    events.send(&dev, "add").context("send without a listener")?;

    chan.send(&uid.to_ne_bytes(), &[])?;
    expect(&chan, b"listening")?;
    let host_sender = devices::Sender::new()?;
    let hint = |action, seq| -> Result<devices::Event> {
        ensure!(
            !poll_readable(&[outside.as_fd()], 0)?[0],
            "private event reached sender namespace"
        );
        host_sender.send(&dev, action, seq)?;
        match outside.receive()? {
            devices::Received::Event(event) => Ok(event),
            other => bail!("unexpected host notification: {other:?}"),
        }
    };
    let mut state = DeviceState::default();
    let mut io = HostDevices {
        host: &host,
        dev_rw: ready.handles.dev_rw.as_fd(),
        init_pidfd: pidfd.as_fd(),
        events: &mut events,
        key: "fixture",
    };
    let mut reconcile = Reconcile {
        state: &mut state,
        io: &mut io,
        policy: &[DeviceClass::Gamepad],
    };
    reconcile.update(Some(hint("add", 42)?));
    ensure!(
        reconcile.state.published.contains_key(&KEY),
        "failed to publish fixture device"
    );
    expect(&chan, b"opened")?;

    fs::remove_file(host.udev_data.join("c1:3"))?;
    fs::remove_file(host.sys_dev_char.join("1:3"))?;
    fs::remove_dir_all("/dev/shm/sys/devices")?;
    // Move the private mount's parent away so the first real detach fails.
    let dev_rw = PathBuf::from(format!("/proc/self/fd/{}", ready.handles.dev_rw.as_raw_fd()));
    fs::rename(dev_rw.join(".psbox"), dev_rw.join("held"))?;
    reconcile.update(Some(hint("remove", 43)?));
    ensure!(reconcile.state.published[&KEY].cleaning_up);
    chan.send(b"check cleanup", &[])?;
    expect(&chan, b"still listening")?;
    fs::rename(dev_rw.join("held"), dev_rw.join(".psbox"))?;
    reconcile.update(None);
    ensure!(reconcile.state.published.is_empty(), "retry left device mounted");
    ensure!(child.wait()?.success(), "libudev listener failed");
    ensure!(
        !poll_readable(&[outside.as_fd()], 0)?[0],
        "private event reached sender namespace"
    );
    Ok(())
}

fn init() -> Result<()> {
    let chan = Seqpacket::from_systemd()?;
    mount_tmpfs("/dev")?;
    fs::create_dir("/dev/.psbox")?;
    let dev_rw = mount::clone_tree(Path::new("/dev"), false)?;
    mount::set_attr_path(Path::new("/dev"), false, MOUNT_ATTR_RDONLY)?;
    mount_tmpfs("/run")?;
    fs::create_dir("/run/udev")?;
    fs::File::create_new("/run/udev/control")?;
    root::Ready {
        handles: root::RootHandles {
            dev_rw,
            root_rw: mount::open_dir(Path::new("/"))?,
        },
        sender: Some(devices::Sender::new()?),
        udev_control: Path::new("/run/udev/control").try_exists()?,
    }
    .send(&chan)?;
    drop(chan);
    ns::drop_all_caps()?;
    Err(Command::new("python3")
        .args(["-I", "-c", include_str!("monitor.py")])
        .exec()
        .into())
}
