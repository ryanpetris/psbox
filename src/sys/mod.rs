//! Thin wrappers over the Linux interfaces the sandbox core is built on.
//!
//! Functions here are safe to call from normal (allocating) code. Code that
//! runs in a forked child of a multithreaded process lives in `spawn.rs` and
//! `inject.rs` and uses raw `libc` calls only.

pub mod cgroup;
pub mod fd;
pub mod inotify;
pub mod landlock;
pub mod mount;
pub mod ns;
pub mod process;
pub mod seccomp;

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Converts a negative raw syscall return into the current `errno`.
pub fn cvt(ret: libc::c_long) -> io::Result<libc::c_long> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

/// Same as [`cvt`] for `c_int` returning libc wrappers.
pub fn cvt_int(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

/// Builds a C string from a path, rejecting interior NUL bytes.
pub fn path_cstr(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

/// Builds a C string from text, rejecting interior NUL bytes.
pub fn cstr(text: &str) -> io::Result<CString> {
    CString::new(text).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "string contains NUL"))
}
