//! Builds the sandbox root and runs the sandbox's PID 1.
//!
//! This code runs in the init child, forked while the supervisor is still
//! single-threaded, so it may allocate freely.

use std::fs;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::plan::{Mount, Plan};
use crate::spec::Network;
use crate::sys::fd::Seqpacket;
use crate::sys::mount::{
    FsContext, MOUNT_ATTR_NODEV, MOUNT_ATTR_NOSUID, MOUNT_ATTR_RDONLY, PointKind, attach, clone_tree, clone_tree_fd,
    make_point, open_dir, open_source, overlay, set_attr_fd, set_attr_path, tmpfs,
};
use crate::sys::ns;

/// Writable views of the read-only root and `/dev` tmpfs mounts. The
/// supervisor keeps these to create mount points and device links without
/// ever making the sandbox's own mounts writable.
pub struct RootHandles {
    pub root_rw: OwnedFd,
    pub dev_rw: OwnedFd,
}

/// Init's mount handles and any private-network notification support.
pub struct Ready {
    pub handles: RootHandles,
    pub sender: Option<crate::devices::Sender>,
    pub udev_control: bool,
}

impl Ready {
    pub fn send(&self, chan: &Seqpacket) -> Result<()> {
        let mut fds = vec![self.handles.root_rw.as_fd(), self.handles.dev_rw.as_fd()];
        if let Some(sender) = &self.sender {
            fds.push(sender.0.as_fd());
        }
        let msg: &[u8] = match (self.sender.is_some(), self.udev_control) {
            (false, false) => b"ready",
            (false, true) => b"ready:udev",
            (true, false) => b"ready:sender",
            (true, true) => b"ready:sender:udev",
        };
        chan.send(msg, &fds)?;
        Ok(())
    }

    pub fn receive(chan: &Seqpacket, private_network: bool) -> Result<Self> {
        let mut buf = [0u8; 4096];
        let (n, mut fds) = chan.recv(&mut buf)?.context("sandbox init exited during setup")?;
        let msg = std::str::from_utf8(&buf[..n]).context("invalid init reply")?;
        let (has_sender, udev_control) = match msg {
            "ready" => (false, false),
            "ready:udev" => (false, true),
            "ready:sender" => (true, false),
            "ready:sender:udev" => (true, true),
            _ => anyhow::bail!("sandbox init failed: {}", msg.strip_prefix("error: ").unwrap_or(msg)),
        };
        if (!private_network && (has_sender || udev_control)) || fds.len() != 2 + usize::from(has_sender) {
            anyhow::bail!("invalid sandbox init ready reply");
        }
        let sender = if has_sender {
            Some(crate::devices::Sender(fds.pop().expect("three descriptors")))
        } else {
            None
        };
        let dev_rw = fds.pop().expect("two mount descriptors");
        let root_rw = fds.pop().expect("root descriptor");
        Ok(Self {
            handles: RootHandles { root_rw, dev_rw },
            sender,
            udev_control,
        })
    }
}

/// Device nodes every sandbox gets.
const BASE_NODES: [&str; 6] = ["null", "zero", "full", "random", "urandom", "tty"];
const DEV_DIRS: [&str; 3] = ["pts", "shm", ".psbox"];
const DEV_LINKS: [(&str, &str); 5] = [
    ("ptmx", "pts/ptmx"),
    ("fd", "/proc/self/fd"),
    ("stdin", "/proc/self/fd/0"),
    ("stdout", "/proc/self/fd/1"),
    ("stderr", "/proc/self/fd/2"),
];

pub fn reserved_dev_name(name: &str) -> bool {
    let first = name.split('/').next().expect("first path component");
    BASE_NODES.contains(&first) || DEV_DIRS.contains(&first) || DEV_LINKS.iter().any(|(name, _)| *name == first)
}

fn rel(dst: &Path) -> &Path {
    dst.strip_prefix("/").unwrap_or(dst)
}

/// Builds the root described by `plan` on a fresh tmpfs mounted at `new_root`.
pub fn build(plan: &Plan, new_root: &Path) -> Result<RootHandles> {
    let tree = tmpfs(0o755, MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV).context("root tmpfs")?;
    attach(tree.as_fd(), open_dir(new_root)?.as_fd()).context("attach root")?;
    let root = open_dir(new_root)?;
    let mut dev_rw = None;
    let mut files = 0;

    for m in &plan.mounts {
        let r = (|| -> Result<()> {
            match m {
                Mount::Bind { src, dst, ro, optional } => bind(
                    root.as_fd(),
                    src,
                    dst,
                    *ro,
                    *optional,
                    plan.source_guard(src).as_deref(),
                )?,
                Mount::Tmpfs { dst, mode } => {
                    let t = tmpfs(*mode, MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV)?;
                    attach(t.as_fd(), make_point(root.as_fd(), rel(dst), PointKind::Dir)?.as_fd())?;
                }
                Mount::Overlay { lower, dst, optional } => {
                    let lower = match open_source(lower, plan.source_guard(lower).as_deref()) {
                        Ok(fd) => fd,
                        Err(e) if e.kind() == io::ErrorKind::NotFound && *optional => return Ok(()),
                        Err(e) => return Err(e).with_context(|| format!("source {}", lower.display())),
                    };
                    let ov = overlay(lower.as_fd(), MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV)?;
                    attach(ov.as_fd(), make_point(root.as_fd(), rel(dst), PointKind::Dir)?.as_fd())?;
                }
                Mount::Symlink { target, dst } => {
                    let path = new_root.join(rel(dst));
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    symlink(target, &path)?;
                }
                Mount::Dir { dst } => fs::create_dir_all(new_root.join(rel(dst)))?,
                Mount::Proc { dst } => {
                    let ctx = FsContext::new("proc")?;
                    let t = ctx.mount(MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV)?;
                    attach(t.as_fd(), make_point(root.as_fd(), rel(dst), PointKind::Dir)?.as_fd())?;
                }
                Mount::Dev { dst } => dev_rw = Some(dev(root.as_fd(), new_root, dst)?),
                Mount::File { dst, content } => {
                    files += 1;
                    file(root.as_fd(), new_root, dst, content, files)?;
                }
            }
            Ok(())
        })();
        r.with_context(|| format!("mount {}", m.dst().display()))?;
    }

    let dev_rw = dev_rw.context("plan has no /dev")?;
    let root_rw = clone_tree(new_root, false).context("clone root")?;
    set_attr_path(
        new_root,
        false,
        MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV,
    )
    .context("make root read-only")?;
    Ok(RootHandles { root_rw, dev_rw })
}

/// Binds the host path `src` at `dst`. Beneath `guard`, no symlink in `src`
/// is followed (see [`open_source`]).
fn bind(
    root: std::os::fd::BorrowedFd<'_>,
    src: &Path,
    dst: &Path,
    ro: bool,
    optional: bool,
    guard: Option<&Path>,
) -> Result<()> {
    let source = match open_source(src, guard) {
        Ok(fd) => fd,
        Err(e) if e.kind() == io::ErrorKind::NotFound && optional => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("source {}", src.display())),
    };
    let meta = fs::File::from(source.try_clone()?).metadata()?;
    let tree = clone_tree_fd(source.as_fd(), true)?;
    let mut attrs = MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV;
    if ro {
        attrs |= MOUNT_ATTR_RDONLY;
    }
    set_attr_fd(tree.as_fd(), true, attrs, 0)?;
    let kind = if meta.is_dir() { PointKind::Dir } else { PointKind::File };
    attach(tree.as_fd(), make_point(root, rel(dst), kind)?.as_fd())?;
    Ok(())
}

/// A read-only file: written to the root tmpfs (still writable during the
/// build), bound to `dst`, then unlinked from the root. When `dst` is a
/// symlink inside the root (such as `/etc/resolv.conf` pointing into
/// `/run/systemd/resolve`), the file goes where the link points.
fn file(root: std::os::fd::BorrowedFd<'_>, new_root: &Path, dst: &Path, content: &str, n: usize) -> Result<()> {
    let target = resolve_in_root(new_root, dst)?;
    let scratch = new_root.join(format!(".psbox-file-{n}"));
    fs::write(&scratch, content)?;
    let tree = clone_tree(&scratch, false)?;
    set_attr_fd(
        tree.as_fd(),
        false,
        MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV,
        0,
    )?;
    attach(tree.as_fd(), make_point(root, rel(&target), PointKind::File)?.as_fd())?;
    fs::remove_file(&scratch)?;
    Ok(())
}

/// Follows symlinks at `path` (a sandbox path) within `new_root`, without
/// requiring the final target to exist.
fn resolve_in_root(new_root: &Path, path: &Path) -> Result<PathBuf> {
    let mut path = path.to_path_buf();
    for _ in 0..16 {
        let Ok(link) = fs::read_link(new_root.join(rel(&path))) else {
            return Ok(path);
        };
        let joined = if link.is_absolute() {
            link
        } else {
            path.parent().unwrap_or(Path::new("/")).join(link)
        };
        let mut clean = PathBuf::from("/");
        for c in joined.components() {
            match c {
                std::path::Component::ParentDir => {
                    clean.pop();
                }
                std::path::Component::Normal(p) => clean.push(p),
                _ => {}
            }
        }
        path = clean;
    }
    anyhow::bail!("too many symlinks at {}", path.display())
}

/// Brings up the loopback interface of a new network namespace.
fn loopback_up() -> Result<()> {
    // SAFETY: an ioctl socket and SIOC[GS]IFFLAGS on a zeroed ifreq naming "lo".
    unsafe {
        let sock = crate::sys::cvt_int(libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0))?;
        let sock = <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(sock);
        let mut req: libc::ifreq = std::mem::zeroed();
        for (d, s) in req.ifr_name.iter_mut().zip(b"lo") {
            *d = *s as libc::c_char;
        }
        use std::os::fd::AsRawFd;
        crate::sys::cvt_int(libc::ioctl(sock.as_raw_fd(), libc::SIOCGIFFLAGS as _, &mut req))?;
        req.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short;
        crate::sys::cvt_int(libc::ioctl(sock.as_raw_fd(), libc::SIOCSIFFLAGS as _, &req))?;
    }
    Ok(())
}

fn dev(root: std::os::fd::BorrowedFd<'_>, new_root: &Path, dst: &Path) -> Result<OwnedFd> {
    let t = tmpfs(0o755, MOUNT_ATTR_NOSUID)?;
    attach(t.as_fd(), make_point(root, rel(dst), PointKind::Dir)?.as_fd())?;
    let dev_path = new_root.join(rel(dst));
    let dev = open_dir(&dev_path)?;

    for node in BASE_NODES {
        let tree = clone_tree(&Path::new("/dev").join(node), false)?;
        set_attr_fd(tree.as_fd(), false, MOUNT_ATTR_NOSUID, 0)?;
        attach(
            tree.as_fd(),
            make_point(dev.as_fd(), Path::new(node), PointKind::File)?.as_fd(),
        )?;
    }
    for dir in DEV_DIRS {
        fs::create_dir(dev_path.join(dir))?;
    }
    let pts = FsContext::new("devpts")?;
    pts.set_string("ptmxmode", "0666")?;
    pts.set_string("mode", "0620")?;
    let pts = pts.mount(MOUNT_ATTR_NOSUID)?;
    attach(pts.as_fd(), open_dir(&dev_path.join("pts"))?.as_fd())?;
    let shm = tmpfs(0o1777, MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV)?;
    attach(shm.as_fd(), open_dir(&dev_path.join("shm"))?.as_fd())?;
    for (name, target) in DEV_LINKS {
        symlink(target, dev_path.join(name))?;
    }

    let rw = clone_tree(&dev_path, false).context("clone /dev")?;
    set_attr_path(&dev_path, false, MOUNT_ATTR_RDONLY | MOUNT_ATTR_NOSUID).context("make /dev read-only")?;
    Ok(rw)
}

/// Entry point of the sandbox's PID 1. Never returns.
///
/// Builds the root in a new mount and IPC namespace, pivots into it, hands
/// the writable root and `/dev` views to the supervisor, drops every
/// capability, applies the same seccomp filter as every other process in
/// the sandbox, and then only reaps orphans.
pub fn run_init(plan: &Plan, new_root: &Path, chan: Seqpacket) -> ! {
    // SAFETY: plain prctl; the init dies with the supervisor. It stays
    // dumpable: the supervisor joins the sandbox's namespaces through it,
    // which needs ptrace access, and the supervisor has no ptrace
    // capability in the user namespace the init's memory belongs to (the
    // host's, since the init never execs). Apps that trace it gain nothing
    // beyond their own privileges: it has no capabilities, no descriptors,
    // and the same Landlock domain and seccomp filter as they do.
    unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) };
    let log = |what: &str, e: &dyn std::fmt::Display| eprintln!("psbox[{}]: init: {what}: {e}", plan.key);
    let setup = || -> Result<Ready> {
        let private_network = plan.network != Network::Host;
        let mut flags = libc::CLONE_NEWNS | libc::CLONE_NEWIPC;
        if private_network {
            flags |= libc::CLONE_NEWNET;
        }
        ns::unshare(flags).context("unshare namespaces")?;
        if private_network {
            loopback_up().context("bring up loopback")?;
        }
        ns::set_propagation(Path::new("/"), libc::MS_PRIVATE).context("make / private")?;
        let sender = if private_network {
            match crate::devices::Sender::new() {
                Ok(sender) => Some(sender),
                Err(e) => {
                    log("create private udev sender", &e);
                    None
                }
            }
        } else {
            None
        };
        let handles = build(plan, new_root)?;
        ns::pivot_into(new_root).context("pivot_root")?;
        let udev_control = private_network
            && match Path::new("/run/udev/control").try_exists() {
                Ok(exists) => exists,
                Err(e) => {
                    log("check sandbox udev control path", &e);
                    false
                }
            };
        Ok(Ready {
            handles,
            sender,
            udev_control,
        })
    };
    match setup() {
        Ok(h) => {
            if let Err(e) = h.send(&chan) {
                log("report ready", &e);
                std::process::exit(1);
            }
        }
        Err(e) => {
            let _ = chan.send(format!("error: {e:#}").as_bytes(), &[]);
            std::process::exit(1);
        }
    }
    drop(chan);
    if let Err(e) = ns::drop_all_caps() {
        log("drop capabilities", &e);
        std::process::exit(1);
    }
    let filter = crate::sys::seccomp::program(plan.allow_32bit);
    let prog = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_ptr() as *mut _,
    };
    // SAFETY: plain syscalls on the calling process; `prog` points at
    // `filter`, which outlives the call.
    let r = unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
        libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32);
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            &prog as *const libc::sock_fprog,
        )
    };
    if r != 0 {
        log("seccomp", &std::io::Error::last_os_error());
        std::process::exit(1);
    }
    reap()
}

/// Reaps children forever. `SIGCHLD` stays blocked so a wakeup is never
/// lost; other signals keep their default disposition, which the kernel
/// ignores for a namespace init when they come from inside the sandbox.
fn reap() -> ! {
    // SAFETY: sigset manipulation on a local set and the calling thread.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGCHLD);
        libc::sigprocmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        loop {
            while libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) > 0 {}
            let mut info: libc::siginfo_t = std::mem::zeroed();
            libc::sigwaitinfo(&set, &mut info);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn ready_handshake_preserves_handles_notification_facts_and_cloexec() {
        for (private, sender, control) in [
            (false, false, false),
            (true, false, false),
            (true, false, true),
            (true, true, false),
            (true, true, true),
        ] {
            let handles = RootHandles {
                root_rw: fs::File::open("/").unwrap().into(),
                dev_rw: fs::File::open("/dev").unwrap().into(),
            };
            let ready = Ready {
                handles,
                sender: sender.then(|| crate::devices::Sender(fs::File::open("/dev/null").unwrap().into())),
                udev_control: control,
            };
            let (a, b) = Seqpacket::pair().unwrap();
            ready.send(&a).unwrap();
            let received = Ready::receive(&b, private).unwrap();
            assert_eq!(received.udev_control, control);
            assert_eq!(received.sender.is_some(), sender);
            let mounts = [
                (ready.handles.root_rw.as_fd(), received.handles.root_rw.as_fd()),
                (ready.handles.dev_rw.as_fd(), received.handles.dev_rw.as_fd()),
            ];
            let sender_fds = ready
                .sender
                .as_ref()
                .zip(received.sender.as_ref())
                .map(|(source, dest)| (source.0.as_fd(), dest.0.as_fd()));
            for (source, dest) in mounts.into_iter().chain(sender_fds) {
                let a = fs::File::from(source.try_clone_to_owned().unwrap()).metadata().unwrap();
                let b = fs::File::from(dest.try_clone_to_owned().unwrap()).metadata().unwrap();
                assert_eq!((a.dev(), a.ino()), (b.dev(), b.ino()));
                // SAFETY: F_GETFD reads flags of a live descriptor.
                assert_ne!(
                    unsafe { libc::fcntl(dest.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
                    0
                );
            }
        }
    }

    #[test]
    fn ready_handshake_validates_flags_and_exact_fd_count() {
        let file = fs::File::open("/dev/null").unwrap();
        for private in [false, true] {
            for (msg, expected_count) in [
                ("ready", Some(2)),
                ("ready:udev", private.then_some(2)),
                ("ready:sender", private.then_some(3)),
                ("ready:sender:udev", private.then_some(3)),
                ("ready:udev:sender", None),
                ("ready:sender:sender", None),
                ("ready:unknown", None),
            ] {
                for count in 0..=4 {
                    let (a, b) = Seqpacket::pair().unwrap();
                    a.send(msg.as_bytes(), &vec![file.as_fd(); count]).unwrap();
                    assert_eq!(
                        Ready::receive(&b, private).is_ok(),
                        expected_count == Some(count),
                        "{msg}, {count} descriptors, private network {private}"
                    );
                }
            }
        }
        let (a, b) = Seqpacket::pair().unwrap();
        a.send(b"error: root setup failed", &[]).unwrap();
        assert!(
            Ready::receive(&b, true)
                .err()
                .unwrap()
                .to_string()
                .contains("root setup failed")
        );
    }
}
