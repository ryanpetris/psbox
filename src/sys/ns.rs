//! Namespace creation, identity maps, and root switching.

use std::fs;
use std::io;
use std::path::Path;

use super::{cvt, cvt_int, path_cstr};

/// Unshares the given `CLONE_NEW*` flags for the calling thread.
///
/// `CLONE_NEWUSER` requires a single-threaded process, so the supervisor
/// calls this before it starts any threads.
pub fn unshare(flags: libc::c_int) -> io::Result<()> {
    // SAFETY: plain syscall.
    cvt_int(unsafe { libc::unshare(flags) })?;
    Ok(())
}

/// Maps only the caller's own uid and gid into its new user namespace.
pub fn write_self_maps(uid: u32, gid: u32) -> io::Result<()> {
    fs::write("/proc/self/setgroups", "deny")?;
    fs::write("/proc/self/uid_map", format!("{uid} {uid} 1\n"))?;
    fs::write("/proc/self/gid_map", format!("{gid} {gid} 1\n"))?;
    Ok(())
}

/// Recursively sets the propagation of `path`.
pub fn set_propagation(path: &Path, flag: libc::c_ulong) -> io::Result<()> {
    let c = path_cstr(path)?;
    // SAFETY: valid path; null source/fstype/data are accepted for propagation changes.
    cvt_int(unsafe {
        libc::mount(
            std::ptr::null(),
            c.as_ptr(),
            std::ptr::null(),
            flag | libc::MS_REC,
            std::ptr::null(),
        )
    })?;
    Ok(())
}

/// Makes the mount at `new_root` the root of the calling mount namespace and
/// detaches the old root.
pub fn pivot_into(new_root: &Path) -> io::Result<()> {
    let c = path_cstr(new_root)?;
    // SAFETY: plain syscalls with valid C strings.
    unsafe {
        cvt_int(libc::chdir(c.as_ptr()))?;
        cvt(libc::syscall(libc::SYS_pivot_root, c".".as_ptr(), c".".as_ptr()))?;
        cvt_int(libc::umount2(c".".as_ptr(), libc::MNT_DETACH))?;
        cvt_int(libc::chdir(c"/".as_ptr()))?;
    }
    Ok(())
}

/// Sends `signal` through a pidfd.
pub fn pidfd_send_signal(pidfd: std::os::fd::BorrowedFd<'_>, signal: libc::c_int) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: plain syscall.
    cvt(unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    })?;
    Ok(())
}

/// Drops every capability from the bounding, effective, permitted, and
/// inheritable sets of the calling thread.
pub fn drop_all_caps() -> io::Result<()> {
    for cap in 0..=63 {
        // SAFETY: plain prctl; EINVAL past the last supported cap ends the loop.
        if unsafe { libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0) } < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
                break;
            }
            return Err(io::Error::last_os_error());
        }
    }
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    let header = Header {
        version: 0x2008_0522,
        pid: 0,
    };
    let data = [Data::default(); 2];
    // SAFETY: capset with a v3 header and two zeroed data words.
    cvt(unsafe { libc::syscall(libc::SYS_capset, &header as *const Header, data.as_ptr()) })?;
    Ok(())
}
