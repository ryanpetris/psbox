//! Attaching and detaching mounts in a running sandbox.
//!
//! The supervisor clones the host tree as a detached mount in its own
//! namespace. A short-lived helper child joins the sandbox's mount namespace
//! (the supervisor owns the user namespace, so this is permitted) and
//! attaches the tree with `move_mount`. `setns` into a mount namespace is
//! refused for multithreaded processes, which is why a helper process does
//! it.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::path::Path;

use crate::sys::mount::{
    MOVE_MOUNT_BENEATH, MOVE_MOUNT_F_EMPTY_PATH, MOVE_MOUNT_T_EMPTY_PATH, OpenHow, RESOLVE_BENEATH, RESOLVE_IN_ROOT,
    RESOLVE_NO_MAGICLINKS, RESOLVE_NO_SYMLINKS,
};
use crate::sys::path_cstr;

/// A sandbox path, split ahead of the fork so the helper does not allocate.
struct Target {
    parent: CString,
    name: CString,
}

impl Target {
    fn new(at: &Path) -> io::Result<Self> {
        let (Some(parent), Some(name)) = (at.parent(), at.file_name()) else {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        };
        Ok(Self {
            parent: path_cstr(parent)?,
            name: path_cstr(Path::new(name))?,
        })
    }
}

enum Op<'a> {
    Attach {
        tree: i32,
        at: &'a Target,
    },
    /// Attach beneath the mount at `at`, then detach that mount: an atomic
    /// swap with no moment where nothing is mounted.
    Replace {
        tree: i32,
        at: &'a Target,
    },
    Detach {
        at: &'a Target,
    },
}

/// Opens `path` beneath `dir` with `O_PATH`, following no symlinks anywhere
/// in it. `dir` is `AT_FDCWD` (the sandbox's root after `setns`) for the
/// parent, and the parent for the last component. Runs in the helper
/// child: raw syscalls only.
unsafe fn open(dir: i32, path: &CString, resolve: u64) -> i32 {
    let how = OpenHow {
        flags: (libc::O_PATH | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: resolve | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS,
    };
    // SAFETY: a valid C string and open_how.
    unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dir,
            path.as_ptr(),
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        ) as i32
    }
}

/// Moves the detached `tree` onto `name` in `dir`, or beneath what is
/// mounted there with `MOVE_MOUNT_BENEATH`. Runs in the helper child.
unsafe fn move_onto(tree: i32, dir: i32, name: &CString, flags: u32) -> bool {
    // SAFETY: valid descriptors and C strings.
    unsafe {
        let target = open(dir, name, RESOLVE_BENEATH);
        target >= 0
            && libc::syscall(
                libc::SYS_move_mount,
                tree,
                c"".as_ptr(),
                target,
                c"".as_ptr(),
                MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH | flags,
            ) == 0
    }
}

/// Lazily detaches the mount on top at `name` in `dir`. `umount2` takes
/// only a path, so the helper changes into `dir` and names the last
/// component without following it. Runs in the helper child.
unsafe fn unmount(dir: i32, name: &CString) -> bool {
    // SAFETY: a valid descriptor and C string; the working directory is the
    // helper's own.
    unsafe { libc::fchdir(dir) == 0 && libc::umount2(name.as_ptr(), libc::MNT_DETACH | libc::UMOUNT_NOFOLLOW) == 0 }
}

fn run(init_pidfd: BorrowedFd<'_>, op: Op<'_>) -> io::Result<()> {
    let pidfd = init_pidfd.as_raw_fd();
    // A raw clone rather than libc's fork: fork runs libc's fork handlers
    // and takes its locks, which another supervisor thread may hold. With
    // only SIGCHLD and no stack this is otherwise the same as fork, and the
    // argument order differences between architectures do not matter.
    // SAFETY: the child only issues raw syscalls on data prepared above.
    let pid = unsafe { libc::syscall(libc::SYS_clone, libc::SIGCHLD as libc::c_long, 0, 0, 0, 0) } as libc::pid_t;
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        // SAFETY: freshly forked child; raw syscalls only.
        unsafe {
            libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
            // Both steps of a replace work in the same parent, opened once,
            // so renaming directories in between cannot split them.
            let ok = libc::setns(pidfd, libc::CLONE_NEWNS) == 0 && {
                let at = match op {
                    Op::Attach { at, .. } | Op::Replace { at, .. } | Op::Detach { at } => at,
                };
                let dir = open(libc::AT_FDCWD, &at.parent, RESOLVE_IN_ROOT);
                dir >= 0
                    && match op {
                        Op::Attach { tree, .. } => move_onto(tree, dir, &at.name, 0),
                        Op::Replace { tree, .. } => {
                            move_onto(tree, dir, &at.name, MOVE_MOUNT_BENEATH) && unmount(dir, &at.name)
                        }
                        Op::Detach { .. } => unmount(dir, &at.name),
                    }
            };
            let code = if ok { 0 } else { *libc::__errno_location() };
            libc::_exit(code & 0xff)
        }
    }
    let mut status = 0;
    // SAFETY: reaping our own helper child.
    if unsafe { libc::waitpid(pid, &mut status, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
        Ok(())
    } else if libc::WIFEXITED(status) {
        Err(io::Error::from_raw_os_error(libc::WEXITSTATUS(status)))
    } else {
        Err(io::Error::other("mount helper was killed"))
    }
}

/// Attaches the detached `tree` at sandbox path `at`, which must exist.
pub fn attach(init_pidfd: BorrowedFd<'_>, tree: BorrowedFd<'_>, at: &Path) -> io::Result<()> {
    let at = Target::new(at)?;
    run(
        init_pidfd,
        Op::Attach {
            tree: tree.as_raw_fd(),
            at: &at,
        },
    )
}

/// Atomically replaces the mount at sandbox path `at` with `tree`.
pub fn replace(init_pidfd: BorrowedFd<'_>, tree: BorrowedFd<'_>, at: &Path) -> io::Result<()> {
    let at = Target::new(at)?;
    run(
        init_pidfd,
        Op::Replace {
            tree: tree.as_raw_fd(),
            at: &at,
        },
    )
}

/// Lazily detaches the mount at sandbox path `at`.
pub fn detach(init_pidfd: BorrowedFd<'_>, at: &Path) -> io::Result<()> {
    let at = Target::new(at)?;
    run(init_pidfd, Op::Detach { at: &at })
}
