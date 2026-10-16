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

/// Whether this process may create a user namespace and, in it, a mount
/// namespace, as every supervisor does at start. A forked child tries and
/// reports the result in its exit status, so the caller is unchanged.
pub fn probe_user_namespace() -> io::Result<()> {
    // SAFETY: the child only issues raw syscalls before `_exit`, so it is
    // safe even if this process has other threads.
    let pid = cvt_int(unsafe { libc::fork() })?;
    if pid == 0 {
        // SAFETY: raw syscalls in the forked child, which never returns. It
        // needs no descriptors, and holding the parent's would keep files
        // it has open alive after the parent lets them go.
        unsafe {
            libc::syscall(libc::SYS_close_range, 0, u32::MAX, 0);
            let code = if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) == 0 {
                0
            } else {
                *libc::__errno_location()
            };
            libc::_exit(code);
        }
    }
    let mut status = 0;
    // SAFETY: waits for the child forked above, into a local.
    while unsafe { libc::waitpid(pid, &mut status, 0) } < 0 {
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
    match (libc::WIFEXITED(status), libc::WEXITSTATUS(status)) {
        (true, 0) => Ok(()),
        (true, errno) => Err(io::Error::from_raw_os_error(errno)),
        _ => Err(io::Error::other("the probe was killed")),
    }
}

/// The running kernel's `major.minor` version, from `uname`.
pub fn kernel_version() -> io::Result<(u32, u32)> {
    // SAFETY: utsname is plain old data filled by uname.
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: uname writes into the local buffer.
    cvt_int(unsafe { libc::uname(&mut uts) })?;
    // SAFETY: uname NUL-terminates every field.
    let release = unsafe { std::ffi::CStr::from_ptr(uts.release.as_ptr()) }.to_string_lossy();
    parse_kernel_version(&release).ok_or_else(|| io::Error::other(format!("unrecognized kernel release {release:?}")))
}

fn parse_kernel_version(release: &str) -> Option<(u32, u32)> {
    let mut parts = release.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_versions_parse() {
        assert_eq!(parse_kernel_version("7.2.8-arch1-2"), Some((7, 2)));
        assert_eq!(parse_kernel_version("7.10.0"), Some((7, 10)));
        assert_eq!(parse_kernel_version("6.18-rc1"), Some((6, 18)));
        assert_eq!(parse_kernel_version("garbage"), None);
    }

    #[test]
    fn probe_leaves_the_caller_alone() {
        let before = fs::read_link("/proc/self/ns/user").unwrap();
        let _ = probe_user_namespace();
        assert_eq!(fs::read_link("/proc/self/ns/user").unwrap(), before);
    }
}
