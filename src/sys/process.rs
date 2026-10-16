//! `clone3` with a pidfd and an initial cgroup.

use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

const CLONE_PIDFD: u64 = 0x1000;
const CLONE_INTO_CGROUP: u64 = 0x2_0000_0000;

#[repr(C)]
#[derive(Default)]
struct CloneArgs {
    flags: u64,
    pidfd: u64,
    child_tid: u64,
    parent_tid: u64,
    exit_signal: u64,
    stack: u64,
    stack_size: u64,
    tls: u64,
    set_tid: u64,
    set_tid_size: u64,
    cgroup: u64,
}

/// The result of [`clone3`] in the parent.
pub struct Cloned {
    pub pid: libc::pid_t,
    pub pidfd: OwnedFd,
}

/// Forks (no shared memory, no new stack) with extra `flags`, placing the
/// child directly in the cgroup `cgroup_dir` and returning a pidfd.
///
/// Returns `Ok(None)` in the child.
///
/// # Safety
/// Same contract as `fork`: in a multithreaded process the child may only
/// call async-signal-safe functions until it execs or exits.
pub unsafe fn clone3(flags: u64, cgroup_dir: RawFd) -> io::Result<Option<Cloned>> {
    let mut pidfd: RawFd = -1;
    let args = CloneArgs {
        flags: flags | CLONE_PIDFD | CLONE_INTO_CGROUP,
        pidfd: &mut pidfd as *mut RawFd as u64,
        exit_signal: libc::SIGCHLD as u64,
        cgroup: cgroup_dir as u64,
        ..Default::default()
    };
    // SAFETY: args is valid for its size; no stack is passed, so the child
    // continues on a copy of the parent's stack like fork.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_clone3,
            &args as *const CloneArgs,
            std::mem::size_of::<CloneArgs>(),
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    if ret == 0 {
        return Ok(None);
    }
    // SAFETY: the kernel stored a new pidfd for the child.
    Ok(Some(Cloned {
        pid: ret as libc::pid_t,
        pidfd: unsafe { OwnedFd::from_raw_fd(pidfd) },
    }))
}
