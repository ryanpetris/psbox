//! On-demand Xwayland for one sandbox.
//!
//! The supervisor creates the sandbox's X socket itself, as a filesystem
//! socket at `/tmp/.X11-unix/X<n>` inside the sandbox, so no abstract socket
//! (which would be shared with the host network namespace) is involved.
//! When the first client connects it starts `xwayland-satellite` with that
//! socket (`-listenfd`), which runs a rootless Xwayland and turns each X11
//! window into a Wayland window on the host compositor.
//!
//! X clients try the abstract socket `@/tmp/.X11-unix/X<n>` before the
//! file, and only fall back to the file when the abstract name does not
//! exist. A sandbox on the host network shares the host's abstract names,
//! and Landlock refuses connections to the host's X servers there, which
//! clients treat as fatal. So the display number is one the host has no
//! abstract X socket for, found by reading `/proc/net/unix`.

use std::collections::HashSet;
use std::fs;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::bus::in_root;
use crate::sys::fd::stream_listen;
use crate::sys::mount::{PointKind, fd_path, make_point, open_dir_in};

pub const SOCKET_DIR: &str = "/tmp/.X11-unix";

/// The first display number tried: "ps" in ASCII (`0x7073`), far from the
/// low numbers host X servers take.
const FIRST_DISPLAY: u32 = 28787;
/// The highest display number an X server accepts (TCP port 6000 + n).
const LAST_DISPLAY: u32 = 59535;

/// A display number the host has no abstract X socket for.
pub fn free_display() -> Result<u32> {
    let table = fs::read_to_string("/proc/net/unix").context("read /proc/net/unix")?;
    pick_display(&table)
}

/// The first display number from [`FIRST_DISPLAY`] that `table` (the
/// contents of `/proc/net/unix`) has no abstract X socket for.
fn pick_display(table: &str) -> Result<u32> {
    let taken: HashSet<u32> = table
        .lines()
        .filter_map(|l| l.split_whitespace().nth(7))
        .filter_map(|p| p.strip_prefix("@/tmp/.X11-unix/X"))
        .filter_map(|n| n.parse().ok())
        .collect();
    match (FIRST_DISPLAY..=LAST_DISPLAY).find(|n| !taken.contains(n)) {
        Some(n) => Ok(n),
        None => bail!("no free X display number between {FIRST_DISPLAY} and {LAST_DISPLAY}"),
    }
}

fn socket(display: u32) -> PathBuf {
    Path::new(SOCKET_DIR).join(format!("X{display}"))
}

/// Creates and listens on the X socket for `display` inside the sandbox,
/// through `root`, a descriptor for the sandbox root. Nothing on the way is
/// followed if it is a symlink.
pub fn listen(root: BorrowedFd<'_>, display: u32) -> Result<OwnedFd> {
    let rel = in_root(Path::new(SOCKET_DIR));
    make_point(root, rel, PointKind::Dir).with_context(|| format!("create {SOCKET_DIR}"))?;
    let dir = open_dir_in(root, rel).with_context(|| format!("open {SOCKET_DIR}"))?;
    // SAFETY: fchmod on a directory fd we own.
    if unsafe { libc::fchmod(dir.as_raw_fd(), 0o1777) } < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| format!("chmod {SOCKET_DIR}"));
    }
    let path = socket(display);
    let name = path.file_name().expect("socket has a name");
    stream_listen(&fd_path(dir.as_fd()).join(name)).with_context(|| format!("listen on {}", path.display()))
}

/// The Wayland socket xwayland-satellite connects to with `xwayland =
/// "builtin"`, in place of the host's. It is in the X socket directory, which
/// is always the sandbox's own tmpfs, so it never lands in a host share.
pub fn decorations_socket(display: u32) -> PathBuf {
    Path::new(SOCKET_DIR).join(format!("psbox-wayland-{display}"))
}

/// Creates and listens on a socket at `path` inside the sandbox, through
/// `root`, in a directory that exists. Nothing on the way is followed if it
/// is a symlink.
pub fn listen_at(root: BorrowedFd<'_>, path: &Path) -> Result<OwnedFd> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        anyhow::bail!("{} is not a socket path", path.display());
    };
    let dir = open_dir_in(root, in_root(parent)).with_context(|| format!("open {}", parent.display()))?;
    stream_listen(&fd_path(dir.as_fd()).join(name)).with_context(|| format!("listen on {}", path.display()))
}

/// The command that serves `display` on the socket passed as descriptor 3.
pub fn satellite_argv(display: u32) -> Vec<String> {
    vec![
        "xwayland-satellite".into(),
        format!(":{display}"),
        "-listenfd".into(),
        "3".into(),
        "-nolisten".into(),
        "local".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "Num       RefCount Protocol Flags    Type St Inode Path\n";

    fn entry(path: &str) -> String {
        format!("0000000000000000: 00000002 00000000 00010000 0001 01 7695802 {path}\n")
    }

    #[test]
    fn picks_the_first_display_the_host_does_not_use() {
        let host = HEADER.to_string()
            + &entry("@/tmp/.X11-unix/X0")
            + &entry("/tmp/.X11-unix/X0")
            + &entry("@/tmp/.X11-unix/X1")
            + "0000000000000000: 00000003 00000000 00000000 0001 03 7736611\n";
        assert_eq!(pick_display(&host).unwrap(), FIRST_DISPLAY);

        // A host socket file of the same number is in the host's /tmp, which
        // the sandbox does not see; only the abstract name collides.
        let file = host + &entry("/tmp/.X11-unix/X28787");
        assert_eq!(pick_display(&file).unwrap(), FIRST_DISPLAY);
        let taken = file + &entry("@/tmp/.X11-unix/X28787");
        assert_eq!(pick_display(&taken).unwrap(), FIRST_DISPLAY + 1);

        let full: String = (FIRST_DISPLAY..=LAST_DISPLAY)
            .map(|n| entry(&format!("@/tmp/.X11-unix/X{n}")))
            .collect();
        assert!(pick_display(&full).is_err());
    }

    #[test]
    fn satellite_serves_the_display() {
        assert_eq!(
            satellite_argv(28787),
            ["xwayland-satellite", ":28787", "-listenfd", "3", "-nolisten", "local"]
        );
        assert_eq!(
            decorations_socket(28787),
            Path::new("/tmp/.X11-unix/psbox-wayland-28787")
        );
        assert_eq!(socket(28787), Path::new("/tmp/.X11-unix/X28787"));
    }
}
