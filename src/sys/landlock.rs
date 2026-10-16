//! The Landlock scope-only domain applied to the supervisor and everything
//! it starts.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use super::cvt;

pub const SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
pub const SCOPE_SIGNAL: u64 = 1 << 1;

const CREATE_RULESET_VERSION: libc::c_uint = 1;

/// The first ABI with abstract-socket and signal scopes.
pub const REQUIRED_ABI: i64 = 6;

/// `struct landlock_ruleset_attr`, first three fields.
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

/// Returns the running kernel's Landlock ABI version.
pub fn abi_version() -> io::Result<i64> {
    // SAFETY: version query takes a null attr and zero size.
    cvt(unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0,
            CREATE_RULESET_VERSION,
        )
    })
}

/// Restricts the calling thread (and its future children) to a domain that
/// handles no filesystem or network rights and only sets `scoped`.
///
/// Because no filesystem access is handled, Landlock does not block mount
/// syscalls for this domain, so the supervisor can still build and change
/// the sandbox's mounts.
pub fn restrict_scopes(scoped: u64) -> io::Result<()> {
    let attr = RulesetAttr {
        handled_access_fs: 0,
        handled_access_net: 0,
        scoped,
    };
    // SAFETY: attr is valid for the size passed.
    let fd = cvt(unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0,
        )
    })?;
    // SAFETY: fd was just returned by the kernel.
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    // SAFETY: restrict_self on a valid ruleset fd.
    cvt(unsafe { libc::syscall(libc::SYS_landlock_restrict_self, fd.as_raw_fd(), 0) })?;
    Ok(())
}
