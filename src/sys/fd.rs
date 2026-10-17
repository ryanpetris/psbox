//! Unix sockets: `SOCK_SEQPACKET` with descriptor passing, stream
//! connections for D-Bus, and nonblocking streams with descriptor passing
//! for Wayland.

use std::io;
use std::mem;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::cvt_int;

/// The largest number of descriptors accepted in one message.
pub const MAX_FDS: usize = 8;

/// A connected or listening `SOCK_SEQPACKET` socket.
#[derive(Debug)]
pub struct Seqpacket(OwnedFd);

/// Builds a socket address for a path, or for an abstract name (which gets
/// a leading NUL and no terminator).
fn sockaddr_bytes(name: &[u8], abstract_name: bool) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    // SAFETY: sockaddr_un is plain old data.
    let mut addr: libc::sockaddr_un = unsafe { mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let offset = usize::from(abstract_name);
    if name.len() + offset >= addr.sun_path.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "socket path too long"));
    }
    for (dst, src) in addr.sun_path[offset..].iter_mut().zip(name) {
        *dst = *src as libc::c_char;
    }
    let len = mem::size_of::<libc::sa_family_t>() + offset + name.len() + usize::from(!abstract_name);
    Ok((addr, len as libc::socklen_t))
}

/// Builds a socket address for a path.
pub fn sockaddr(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    sockaddr_bytes(path.as_os_str().as_bytes(), false)
}

fn new_socket_of(kind: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: plain socket syscall.
    let fd = cvt_int(unsafe { libc::socket(libc::AF_UNIX, kind | libc::SOCK_CLOEXEC, 0) })?;
    // SAFETY: fd was just created.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn new_socket() -> io::Result<OwnedFd> {
    new_socket_of(libc::SOCK_SEQPACKET)
}

/// Binds and listens on a `SOCK_STREAM` Unix socket at `path`.
pub fn stream_listen(path: &Path) -> io::Result<OwnedFd> {
    let fd = new_socket_of(libc::SOCK_STREAM)?;
    let (addr, len) = sockaddr(path)?;
    // SAFETY: addr is a valid sockaddr_un of length len.
    cvt_int(unsafe { libc::bind(fd.as_raw_fd(), (&addr as *const libc::sockaddr_un).cast(), len) })?;
    // SAFETY: plain syscall.
    cvt_int(unsafe { libc::listen(fd.as_raw_fd(), 64) })?;
    Ok(fd)
}

/// Connects a `SOCK_STREAM` Unix socket to a path, or to an abstract name
/// when `abstract_name` is set.
pub fn stream_connect(name: &[u8], abstract_name: bool) -> io::Result<OwnedFd> {
    let fd = new_socket_of(libc::SOCK_STREAM)?;
    let (addr, len) = sockaddr_bytes(name, abstract_name)?;
    // SAFETY: addr is a valid sockaddr_un of length len.
    cvt_int(unsafe { libc::connect(fd.as_raw_fd(), (&addr as *const libc::sockaddr_un).cast(), len) })?;
    Ok(fd)
}

impl Seqpacket {
    /// The listening socket systemd passed through socket activation
    /// (`LISTEN_FDS=1`, descriptor 3).
    pub fn from_systemd() -> io::Result<Self> {
        let ours = std::env::var("LISTEN_PID").ok().and_then(|p| p.parse::<u32>().ok()) == Some(std::process::id());
        if !ours || std::env::var("LISTEN_FDS").as_deref() != Ok("1") {
            return Err(io::Error::other(
                "no socket from systemd; the supervisor is started by its psbox@.socket unit",
            ));
        }
        // SAFETY: systemd passed descriptor 3 to this process.
        unsafe {
            cvt_int(libc::fcntl(3, libc::F_SETFD, libc::FD_CLOEXEC))?;
            Ok(Self(OwnedFd::from_raw_fd(3)))
        }
    }

    pub fn connect(path: &Path) -> io::Result<Self> {
        let fd = new_socket()?;
        let (addr, len) = sockaddr(path)?;
        // SAFETY: addr is a valid sockaddr_un of length len.
        cvt_int(unsafe { libc::connect(fd.as_raw_fd(), (&addr as *const libc::sockaddr_un).cast(), len) })?;
        Ok(Self(fd))
    }

    pub fn accept(&self) -> io::Result<Self> {
        // SAFETY: null address output is allowed.
        let fd = cvt_int(unsafe {
            libc::accept4(
                self.0.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC,
            )
        })?;
        // SAFETY: fd was just returned.
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    pub fn pair() -> io::Result<(Self, Self)> {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: fds has room for two descriptors.
        cvt_int(unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        })?;
        // SAFETY: both fds were just created.
        Ok(unsafe { (Self(OwnedFd::from_raw_fd(fds[0])), Self(OwnedFd::from_raw_fd(fds[1]))) })
    }

    /// The peer's uid from `SO_PEERCRED`.
    pub fn peer_uid(&self) -> io::Result<u32> {
        // SAFETY: ucred is plain old data.
        let mut cred: libc::ucred = unsafe { mem::zeroed() };
        let mut len = mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: cred/len describe a valid output buffer.
        cvt_int(unsafe {
            libc::getsockopt(
                self.0.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        })?;
        Ok(cred.uid)
    }

    /// Sends one message with optional descriptors.
    pub fn send(&self, data: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
        assert!(fds.len() <= MAX_FDS);
        let mut iov = libc::iovec {
            iov_base: data.as_ptr() as *mut _,
            iov_len: data.len(),
        };
        // SAFETY: msghdr is plain old data.
        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        // u64 storage: control messages start with a `cmsghdr`, which needs
        // more than byte alignment.
        let mut cmsg_buf = [0u64; 32];
        if !fds.is_empty() {
            let bytes = mem::size_of_val(fds) as u32;
            // SAFETY: CMSG_SPACE is a pure computation.
            let space = unsafe { libc::CMSG_SPACE(bytes) } as usize;
            msg.msg_control = cmsg_buf.as_mut_ptr().cast();
            msg.msg_controllen = space as _;
            // SAFETY: msg_control points at a buffer of `space` bytes.
            unsafe {
                let cmsg = libc::CMSG_FIRSTHDR(&msg);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = libc::CMSG_LEN(bytes) as _;
                let dst = libc::CMSG_DATA(cmsg) as *mut RawFd;
                for (i, fd) in fds.iter().enumerate() {
                    dst.add(i).write_unaligned(fd.as_raw_fd());
                }
            }
        }
        // SAFETY: msg is fully initialized.
        let n = unsafe { libc::sendmsg(self.0.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Receives one message. Returns `Ok(None)` at end of stream.
    pub fn recv(&self, buf: &mut [u8]) -> io::Result<Option<(usize, Vec<OwnedFd>)>> {
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr().cast(),
            iov_len: buf.len(),
        };
        // SAFETY: msghdr is plain old data.
        let mut msg: libc::msghdr = unsafe { mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        let mut cmsg_buf = [0u64; 32];
        msg.msg_control = cmsg_buf.as_mut_ptr().cast();
        msg.msg_controllen = mem::size_of_val(&cmsg_buf) as _;
        // SAFETY: msg points at valid buffers.
        let n = unsafe { libc::recvmsg(self.0.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut fds = Vec::new();
        // SAFETY: walking the control buffer the kernel just filled.
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                    let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / mem::size_of::<RawFd>();
                    for i in 0..count {
                        fds.push(OwnedFd::from_raw_fd(data.add(i).read_unaligned()));
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }
        if msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "truncated message"));
        }
        if n == 0 && fds.is_empty() {
            return Ok(None);
        }
        if fds.len() > MAX_FDS {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "too many descriptors"));
        }
        Ok(Some((n as usize, fds)))
    }
}

impl AsFd for Seqpacket {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl AsRawFd for Seqpacket {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

/// Waits until `fd` is readable or the timeout (ms, -1 = forever) passes.
/// Returns readiness per descriptor.
pub fn poll_readable(fds: &[BorrowedFd<'_>], timeout_ms: i32) -> io::Result<Vec<bool>> {
    let mut pfds: Vec<libc::pollfd> = fds
        .iter()
        .map(|fd| libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    loop {
        // SAFETY: pfds is a valid array.
        let r = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout_ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        return Ok(pfds.iter().map(|p| p.revents != 0).collect());
    }
}

/// Accepts a connection on a listening stream socket.
pub fn accept_stream(listener: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    // SAFETY: null address output is allowed.
    let fd = cvt_int(unsafe {
        libc::accept4(
            listener.as_raw_fd(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            libc::SOCK_CLOEXEC,
        )
    })?;
    // SAFETY: fd was just returned.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The pid of a connected socket's peer, from `SO_PEERCRED`, in the
/// caller's pid namespace.
pub fn peer_pid(fd: BorrowedFd<'_>) -> io::Result<libc::pid_t> {
    // SAFETY: ucred is plain old data.
    let mut cred: libc::ucred = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred/len describe a valid output buffer.
    cvt_int(unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    })?;
    Ok(cred.pid)
}

/// The most descriptors one Wayland `sendmsg` carries, which is also what
/// Wayland libraries accept in one read.
pub const STREAM_MAX_FDS: usize = 28;

/// Sends what a stream socket takes of `data` without blocking, with `fds`
/// (at most [`STREAM_MAX_FDS`]) attached to the first byte. Returns how many
/// bytes went out; `WouldBlock` when none did.
pub fn send_stream(fd: BorrowedFd<'_>, data: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<usize> {
    if fds.len() > STREAM_MAX_FDS || data.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad descriptor batch"));
    }
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut _,
        iov_len: data.len(),
    };
    // SAFETY: msghdr is plain old data.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    let mut cmsg_buf = [0u64; 32];
    if !fds.is_empty() {
        let bytes = mem::size_of_val(fds) as u32;
        // SAFETY: CMSG_SPACE is a pure computation.
        let space = unsafe { libc::CMSG_SPACE(bytes) } as usize;
        msg.msg_control = cmsg_buf.as_mut_ptr().cast();
        msg.msg_controllen = space as _;
        // SAFETY: msg_control points at a buffer of at least `space` bytes
        // (28 descriptors need 128).
        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(bytes) as _;
            let dst = libc::CMSG_DATA(cmsg) as *mut RawFd;
            for (i, fd) in fds.iter().enumerate() {
                dst.add(i).write_unaligned(fd.as_raw_fd());
            }
        }
    }
    // SAFETY: msg is fully initialized.
    let n = unsafe { libc::sendmsg(fd.as_raw_fd(), &msg, libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(n as usize)
}

/// Receives what a stream socket has, up to `buf.len()` bytes, without
/// blocking, appending the descriptors that came with it to `fds`. `Ok(0)`
/// is the end of the stream; `WouldBlock` means nothing is waiting.
pub fn recv_stream(fd: BorrowedFd<'_>, buf: &mut [u8], fds: &mut Vec<OwnedFd>) -> io::Result<usize> {
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    // SAFETY: msghdr is plain old data.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    let mut cmsg_buf = [0u64; 32];
    msg.msg_control = cmsg_buf.as_mut_ptr().cast();
    msg.msg_controllen = mem::size_of_val(&cmsg_buf) as _;
    // SAFETY: msg points at valid buffers.
    let n = unsafe { libc::recvmsg(fd.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC | libc::MSG_DONTWAIT) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: walking the control buffer the kernel just filled.
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / mem::size_of::<RawFd>();
                for i in 0..count {
                    fds.push(OwnedFd::from_raw_fd(data.add(i).read_unaligned()));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "too many descriptors"));
    }
    Ok(n as usize)
}

/// Waits until one of `fds` has one of the `poll` events asked for with it.
/// Returns the events that happened, per descriptor.
pub fn poll_events(fds: &[(BorrowedFd<'_>, libc::c_short)], timeout_ms: i32) -> io::Result<Vec<libc::c_short>> {
    let mut pfds: Vec<libc::pollfd> = fds
        .iter()
        .map(|(fd, events)| libc::pollfd {
            fd: fd.as_raw_fd(),
            events: *events,
            revents: 0,
        })
        .collect();
    loop {
        // SAFETY: pfds is a valid array.
        let r = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout_ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        return Ok(pfds.iter().map(|p| p.revents).collect());
    }
}
