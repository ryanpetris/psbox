//! The connector: a process in the sandbox's PID namespace that opens the
//! gateway's host bus connections.
//!
//! The host portal identifies a caller by the process that opened its bus
//! connection, and reads process ids (Realtime) in that process's PID
//! namespace, as it does for Flatpak apps. The supervisor is outside the
//! sandbox's PID namespace, so the connector, forked on the forker thread,
//! opens each host connection instead: for every request on its control
//! socket it connects a new socket to the host bus and passes it back. It
//! lives as long as the sandbox, because the portal looks up its namespace
//! when a call needs it.
//!
//! The connector joins the sandbox's mount namespace (and network
//! namespace, when it has one), so its `/proc` entries show only what the
//! sandbox already sees. It reaches the host bus through a descriptor for
//! the socket opened before the fork, which it keeps along with its control
//! socket; every other descriptor is closed. It keeps the supervisor's
//! capabilities: the kernel lets a process of the same user ptrace another,
//! or open the `/proc` entries that hold its descriptors and memory, only
//! when it holds every capability the other has, so sandbox processes,
//! which hold none, cannot. The host portal can, as the owner of the
//! supervisor's user namespace, and it reads the connector's `/proc` entry
//! when the supervisor registers a client. A core dump limit of one byte
//! means a signal from the sandbox cannot make it dump the supervisor's
//! memory on the host.
//!
//! The connector is forked from the multithreaded supervisor and never
//! execs, so it uses only raw syscalls on data prepared before the fork.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::Path;

use crate::forker::Forker;
use crate::sys::fd::{Seqpacket, poll_readable, sockaddr};
use crate::sys::ns;
use crate::sys::process::clone3;

/// How long a request may take before the connector is replaced.
const REPLY_TIMEOUT_MS: i32 = 5000;

pub struct Connector {
    ctl: Seqpacket,
    pub pid: libc::pid_t,
    pidfd: OwnedFd,
}

impl Connector {
    /// Forks the connector into the sandbox's PID namespace (through
    /// `forker`) and the cgroup `cgroup`, joined to the namespaces of the
    /// sandbox init `init_pidfd` (mount, and network with `join_net`), to
    /// connect to the host bus socket `bus`.
    pub fn start(forker: &Forker, bus: &Path, init_pidfd: RawFd, join_net: bool, cgroup: &Path) -> io::Result<Self> {
        let bus = open_socket(bus)?;
        let (addr, len) = sockaddr(Path::new(&format!("/proc/self/fd/{}", bus.as_raw_fd())))?;
        let mut join = libc::CLONE_NEWNS;
        if join_net {
            join |= libc::CLONE_NEWNET;
        }
        let (ctl, child_ctl) = Seqpacket::pair()?;
        let cgroup = OwnedFd::from(std::fs::File::open(cgroup)?);
        let cloned = forker.run(move || {
            // SAFETY: the child runs only `serve`, which issues raw syscalls
            // on data prepared before the fork.
            match unsafe { clone3(0, cgroup.as_raw_fd()) } {
                Ok(None) => serve(
                    child_ctl.as_raw_fd(),
                    bus.as_raw_fd(),
                    Some((init_pidfd, join)),
                    &addr,
                    len,
                ),
                other => other,
            }
        })??;
        let cloned = cloned.expect("clone3 returns the child only in the child");
        Ok(Self {
            ctl,
            pid: cloned.pid,
            pidfd: cloned.pidfd,
        })
    }

    /// Asks for a new socket connected to the host bus, not yet
    /// authenticated. The outer error means the connector failed (it
    /// exited, or did not answer in time) and must be replaced; the inner
    /// one is the connector's failure to connect.
    pub fn connect(&self) -> io::Result<io::Result<OwnedFd>> {
        self.ctl.send(&[0], &[])?;
        if !poll_readable(&[self.ctl.as_fd()], REPLY_TIMEOUT_MS)?[0] {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "the connector did not answer"));
        }
        let mut errno = [0u8; 4];
        let (n, mut fds) = self
            .ctl
            .recv(&mut errno)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "the connector exited"))?;
        match i32::from_ne_bytes(errno) {
            0 if n == errno.len() && fds.len() == 1 => Ok(Ok(fds.remove(0))),
            0 => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bad reply from the connector",
            )),
            e => Ok(Err(io::Error::from_raw_os_error(e))),
        }
    }

    /// The connector's pidfd, for reaping it.
    pub fn pidfd(&self) -> BorrowedFd<'_> {
        self.pidfd.as_fd()
    }

    /// Kills the connector (whoever waits on its pidfd reaps it).
    pub fn kill(&self) {
        let _ = ns::pidfd_send_signal(self.pidfd.as_fd(), libc::SIGKILL);
    }
}

/// An `O_PATH` descriptor for the socket at `path`, which `connect` accepts
/// through `/proc/self/fd`.
fn open_socket(path: &Path) -> io::Result<OwnedFd> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH)
        .open(path)?;
    Ok(file.into())
}

/// The connector's main loop, in the forked child: joins the namespaces in
/// `join` (a pidfd and `setns` flags), then answers each request byte on
/// `ctl` with a new socket connected to `addr`, until the supervisor closes
/// its end. `bus` is the descriptor `addr` names.
fn serve(
    ctl: RawFd,
    bus: RawFd,
    join: Option<(RawFd, libc::c_int)>,
    addr: &libc::sockaddr_un,
    len: libc::socklen_t,
) -> ! {
    let no_core = libc::rlimit64 {
        rlim_cur: 1,
        rlim_max: 1,
    };
    let (low, high) = (ctl.min(bus) as u32, ctl.max(bus) as u32);

    // SAFETY: raw syscalls on stack data and on descriptors this process
    // owns; nothing here allocates or takes a lock.
    unsafe {
        // The forker thread lives as long as the supervisor. A core limit
        // of 1 stops pipe handlers too, and the hard limit cannot be raised
        // again without CAP_SYS_RESOURCE on the host.
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) < 0
            || libc::prctl(libc::PR_SET_NAME, c"psbox-connector".as_ptr(), 0, 0, 0) < 0
            || libc::syscall(
                libc::SYS_prlimit64,
                0,
                libc::RLIMIT_CORE,
                &no_core as *const libc::rlimit64,
                std::ptr::null_mut::<libc::rlimit64>(),
            ) < 0
            || join.is_some_and(|(pidfd, flags)| libc::syscall(libc::SYS_setns, pidfd, flags) < 0)
            || (low > 0 && libc::syscall(libc::SYS_close_range, 0, low - 1, 0) < 0)
            || (high > low + 1 && libc::syscall(libc::SYS_close_range, low + 1, high - 1, 0) < 0)
            || libc::syscall(libc::SYS_close_range, high + 1, u32::MAX, 0) < 0
        {
            libc::_exit(1);
        }

        loop {
            let mut request = 0u8;
            let n = libc::recv(ctl, (&mut request as *mut u8).cast(), 1, 0);
            if n == 0 {
                libc::_exit(0);
            }
            if n < 0 {
                if *libc::__errno_location() == libc::EINTR {
                    continue;
                }
                libc::_exit(1);
            }

            let sock = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
            let mut errno = 0;
            if sock < 0 {
                errno = *libc::__errno_location();
            } else {
                while libc::connect(sock, (addr as *const libc::sockaddr_un).cast(), len) < 0 {
                    errno = *libc::__errno_location();
                    if errno != libc::EINTR {
                        break;
                    }
                    errno = 0;
                }
            }
            let sent = if errno == 0 {
                send(ctl, 0, sock)
            } else {
                send(ctl, errno, -1)
            };
            if sock >= 0 {
                libc::close(sock);
            }
            if !sent {
                libc::_exit(1);
            }
        }
    }
}

/// Sends `errno` and, when `fd` is not -1, the descriptor `fd` on `ctl`.
///
/// # Safety
/// Raw syscalls only, so it may run in the forked child.
unsafe fn send(ctl: RawFd, errno: i32, fd: RawFd) -> bool {
    let data = errno.to_ne_bytes();
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut _,
        iov_len: data.len(),
    };
    // SAFETY: msghdr is plain old data.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    // u64 storage: a control message starts with a `cmsghdr`.
    let mut cmsg_buf = [0u64; 4];
    if fd != -1 {
        let bytes = std::mem::size_of::<RawFd>() as u32;
        // SAFETY: CMSG_SPACE and CMSG_LEN are pure computations, and
        // msg_control points at a buffer larger than CMSG_SPACE(bytes).
        unsafe {
            msg.msg_control = cmsg_buf.as_mut_ptr().cast();
            msg.msg_controllen = libc::CMSG_SPACE(bytes) as _;
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            (*cmsg).cmsg_len = libc::CMSG_LEN(bytes) as _;
            (libc::CMSG_DATA(cmsg) as *mut RawFd).write_unaligned(fd);
        }
    }
    loop {
        // SAFETY: msg is fully initialized and its buffers outlive the call.
        let n = unsafe { libc::sendmsg(ctl, &msg, libc::MSG_NOSIGNAL) };
        if n >= 0 {
            return true;
        }
        // SAFETY: reading this thread's errno.
        if unsafe { *libc::__errno_location() } != libc::EINTR {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd;

    use crate::spawn;
    use crate::sys::fd::stream_listen;

    #[test]
    fn connects_to_the_bus_socket_through_its_descriptor() {
        let dir = std::env::temp_dir().join(format!("psbox-connector-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bus");
        let listener = stream_listen(&path).unwrap();

        // As `start` does, without the namespaces and the cgroup.
        let bus = open_socket(&path).unwrap();
        let (addr, len) = sockaddr(Path::new(&format!("/proc/self/fd/{}", bus.as_raw_fd()))).unwrap();
        let (ctl, child_ctl) = Seqpacket::pair().unwrap();
        // SAFETY: the child runs only `serve`, which issues raw syscalls.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            serve(child_ctl.as_raw_fd(), bus.as_raw_fd(), None, &addr, len);
        }
        drop(child_ctl);
        // SAFETY: pidfd_open on our own unreaped child.
        let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        assert!(pidfd >= 0);
        let connector = Connector {
            ctl,
            pid,
            // SAFETY: pidfd_open returned a new descriptor.
            pidfd: unsafe { OwnedFd::from_raw_fd(pidfd as RawFd) },
        };

        // The socket's path is gone; its descriptor still reaches it.
        std::fs::remove_file(&path).unwrap();
        for _ in 0..2 {
            let sock = connector.connect().unwrap().unwrap();
            // SAFETY: null address output is allowed.
            let peer = unsafe { libc::accept4(listener.as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut(), 0) };
            assert!(peer >= 0);
            // SAFETY: peer was just accepted.
            unsafe { libc::close(peer) };
            drop(sock);
        }

        drop(listener);
        let e = connector.connect().unwrap().unwrap_err();
        assert_eq!(e.raw_os_error(), Some(libc::ECONNREFUSED));

        // Closing the control socket ends it cleanly.
        drop(connector.ctl);
        assert!(matches!(
            spawn::wait(connector.pidfd.as_fd()).unwrap(),
            spawn::Exit::Code(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
