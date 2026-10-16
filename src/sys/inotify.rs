//! A small inotify wrapper for watching directories.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

use super::{cvt_int, path_cstr};

pub struct Inotify(OwnedFd);

/// One event: the watch it came from, its mask, and the entry name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub wd: i32,
    pub mask: u32,
    pub name: String,
}

impl Inotify {
    pub fn new() -> io::Result<Self> {
        // SAFETY: plain syscall.
        let fd = cvt_int(unsafe { libc::inotify_init1(libc::IN_CLOEXEC) })?;
        // SAFETY: fd was just created.
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    /// Watches `path` for `mask`, returning the watch descriptor.
    pub fn add(&self, path: &Path, mask: u32) -> io::Result<i32> {
        let c = path_cstr(path)?;
        // SAFETY: valid fd and path.
        cvt_int(unsafe { libc::inotify_add_watch(self.0.as_raw_fd(), c.as_ptr(), mask) })
    }

    /// Blocks for the next batch of events.
    pub fn read(&self) -> io::Result<Vec<Event>> {
        let mut buf = [0u8; 8192];
        let n = loop {
            // SAFETY: reading into a local buffer.
            let n = unsafe { libc::read(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n >= 0 {
                break n as usize;
            }
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::EINTR) {
                return Err(e);
            }
        };
        Ok(parse(&buf[..n]))
    }
}

pub fn parse(buf: &[u8]) -> Vec<Event> {
    let header = std::mem::size_of::<libc::inotify_event>();
    let mut out = Vec::new();
    let mut off = 0;
    while off + header <= buf.len() {
        // SAFETY: bounds checked above; read_unaligned copes with packing.
        let ev: libc::inotify_event = unsafe { std::ptr::read_unaligned(buf[off..].as_ptr().cast()) };
        let end = (off + header + ev.len as usize).min(buf.len());
        let raw = &buf[off + header..end];
        let name = String::from_utf8_lossy(raw.split(|b| *b == 0).next().unwrap_or_default()).into_owned();
        out.push(Event {
            wd: ev.wd,
            mask: ev.mask,
            name,
        });
        off = end;
    }
    out
}
