//! A blocking D-Bus client connection with descriptor passing.
//!
//! One thread reads (`recv`) while any thread may send (`send`). `call` is
//! for setup, before a reader loop owns the connection.

use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use super::message::{self, Message, frame_len};
use super::value::Value;
use crate::sys::fd::stream_connect;

/// Descriptors accepted per received chunk (the kernel's SCM_MAX_FD).
const MAX_FDS_PER_CHUNK: usize = 253;

pub const BUS_NAME: &str = "org.freedesktop.DBus";
pub const BUS_PATH: &str = "/org/freedesktop/DBus";

#[derive(Default)]
struct ReadState {
    buf: Vec<u8>,
    fds: VecDeque<OwnedFd>,
}

pub struct Conn {
    sock: OwnedFd,
    write: Mutex<()>,
    read: Mutex<ReadState>,
    serial: AtomicU32,
    unique: String,
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Decodes `%xx` escapes in a D-Bus address value.
pub fn unescape(v: &str) -> Vec<u8> {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(hex) = b.get(i + 1..i + 3)
            && hex.iter().all(u8::is_ascii_hexdigit)
        {
            let digit = |c: u8| (c as char).to_digit(16).expect("hex digit") as u8;
            out.push(digit(hex[0]) << 4 | digit(hex[1]));
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Opens a stream to the first usable `unix:` entry of a D-Bus address.
fn open_address(address: &str) -> io::Result<OwnedFd> {
    let mut last = invalid(format!("no usable unix address in {address:?}"));
    for entry in address.split(';') {
        let Some(params) = entry.strip_prefix("unix:") else {
            continue;
        };
        for kv in params.split(',') {
            let r = match kv.split_once('=') {
                Some(("path", v)) => stream_connect(&unescape(v), false),
                Some(("abstract", v)) => stream_connect(&unescape(v), true),
                _ => continue,
            };
            match r {
                Ok(fd) => return Ok(fd),
                Err(e) => last = e,
            }
        }
    }
    Err(last)
}

impl Conn {
    /// Connects to a bus address, authenticates, and says `Hello`.
    pub fn connect(address: &str) -> io::Result<Self> {
        Self::setup(open_address(address)?)
    }

    /// Connects to a bus socket path.
    pub fn connect_path(path: &Path) -> io::Result<Self> {
        Self::setup(stream_connect(path.as_os_str().as_bytes(), false)?)
    }

    fn setup(sock: OwnedFd) -> io::Result<Self> {
        auth(&sock)?;
        let mut conn = Self {
            sock,
            write: Mutex::new(()),
            read: Mutex::new(ReadState::default()),
            serial: AtomicU32::new(1),
            unique: String::new(),
        };
        let reply = conn.call(&Message::method_call(BUS_NAME, BUS_PATH, BUS_NAME, "Hello", &[]))?;
        conn.unique = reply
            .args()
            .ok()
            .and_then(|a| a.into_iter().next())
            .and_then(|v| v.as_str().map(str::to_string))
            .ok_or_else(|| invalid("bad Hello reply"))?;
        Ok(conn)
    }

    /// This connection's unique bus name.
    pub fn unique_name(&self) -> &str {
        &self.unique
    }

    /// Sends a message (its `serial` is assigned here) and returns the serial.
    pub fn send(&self, msg: &Message, fds: &[BorrowedFd<'_>]) -> io::Result<u32> {
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        let mut m = msg.clone();
        m.unix_fds = fds.len() as u32;
        let bytes = m.encode(serial);
        let _guard = self.write.lock().unwrap();
        let mut sent = 0;
        while sent < bytes.len() {
            let chunk_fds = if sent == 0 { fds } else { &[] };
            sent += sendmsg(&self.sock, &bytes[sent..], chunk_fds)?;
        }
        Ok(serial)
    }

    /// Blocks until a whole message arrives.
    pub fn recv(&self) -> io::Result<(Message, Vec<OwnedFd>)> {
        let mut st = self.read.lock().unwrap();
        loop {
            if st.buf.len() >= 16 {
                let prefix: [u8; 16] = st.buf[..16].try_into().expect("16 bytes");
                let total = frame_len(&prefix).map_err(|e| invalid(e.to_string()))?;
                if st.buf.len() >= total {
                    let raw: Vec<u8> = st.buf.drain(..total).collect();
                    let msg = Message::parse(&raw).map_err(|e| invalid(e.to_string()))?;
                    let n = msg.unix_fds as usize;
                    if st.fds.len() < n {
                        return Err(invalid("message references missing descriptors"));
                    }
                    let fds = st.fds.drain(..n).collect();
                    return Ok((msg, fds));
                }
            }
            let ReadState { buf, fds } = &mut *st;
            if recvmsg(&self.sock, buf, fds)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "bus closed the connection",
                ));
            }
        }
    }

    /// Sends a method call and waits for its reply, discarding anything
    /// else that arrives meanwhile. Only for setup.
    pub fn call(&self, msg: &Message) -> io::Result<Message> {
        let serial = self.send(msg, &[])?;
        loop {
            let (reply, _) = self.recv()?;
            if reply.reply_serial != Some(serial) {
                continue;
            }
            if reply.kind == message::ERROR {
                let text = reply
                    .args()
                    .ok()
                    .and_then(|a| a.first().and_then(|v| v.as_str().map(str::to_string)));
                return Err(io::Error::other(format!(
                    "{}: {}",
                    reply.error_name.as_deref().unwrap_or("error"),
                    text.unwrap_or_default()
                )));
            }
            return Ok(reply);
        }
    }

    /// Calls a method on the bus daemon itself.
    pub fn bus_call(&self, member: &str, args: &[Value]) -> io::Result<Message> {
        self.call(&Message::method_call(BUS_NAME, BUS_PATH, BUS_NAME, member, args))
    }

    /// Unblocks a thread waiting in `recv`.
    pub fn shutdown(&self) {
        // SAFETY: shutdown on a valid socket.
        unsafe { libc::shutdown(self.sock.as_raw_fd(), libc::SHUT_RDWR) };
    }
}

fn auth(sock: &OwnedFd) -> io::Result<()> {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let hex: String = uid.to_string().bytes().map(|b| format!("{b:02x}")).collect();
    write_all(sock, format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())?;
    let line = read_line(sock)?;
    if !line.starts_with("OK ") {
        return Err(invalid(format!("authentication rejected: {line}")));
    }
    write_all(sock, b"NEGOTIATE_UNIX_FD\r\n")?;
    let line = read_line(sock)?;
    if line != "AGREE_UNIX_FD" {
        return Err(invalid(format!("bus refused descriptor passing: {line}")));
    }
    write_all(sock, b"BEGIN\r\n")
}

fn write_all(sock: &OwnedFd, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        let n = sendmsg(sock, data, &[])?;
        data = &data[n..];
    }
    Ok(())
}

/// Reads one CRLF-terminated line one byte at a time, so nothing past the
/// line is consumed before the binary protocol starts.
fn read_line(sock: &OwnedFd) -> io::Result<String> {
    let mut line = Vec::new();
    loop {
        let mut b = 0u8;
        // SAFETY: reading one byte into a local.
        let n = unsafe { libc::recv(sock.as_raw_fd(), (&mut b as *mut u8).cast(), 1, 0) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "bus closed during authentication",
            ));
        }
        line.push(b);
        if line.ends_with(b"\r\n") {
            line.truncate(line.len() - 2);
            return String::from_utf8(line).map_err(|_| invalid("non-UTF-8 auth reply"));
        }
        if line.len() > 4096 {
            return Err(invalid("auth line too long"));
        }
    }
}

fn sendmsg(sock: &OwnedFd, data: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<usize> {
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut _,
        iov_len: data.len(),
    };
    // SAFETY: msghdr is plain old data.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    // u64 storage: control messages start with a `cmsghdr`, which needs more
    // than byte alignment.
    let mut cmsg_buf = vec![0u64; (64 + fds.len() * 4).div_ceil(8)];
    if !fds.is_empty() {
        let bytes = std::mem::size_of_val(fds) as u32;
        // SAFETY: control buffer is large enough for one SCM_RIGHTS header.
        unsafe {
            msg.msg_control = cmsg_buf.as_mut_ptr().cast();
            msg.msg_controllen = libc::CMSG_SPACE(bytes) as _;
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
    loop {
        // SAFETY: msg is fully initialized.
        let n = unsafe { libc::sendmsg(sock.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        return Ok(n as usize);
    }
}

fn recvmsg(sock: &OwnedFd, buf: &mut Vec<u8>, fds: &mut VecDeque<OwnedFd>) -> io::Result<usize> {
    let mut chunk = vec![0u8; 64 * 1024];
    let mut iov = libc::iovec {
        iov_base: chunk.as_mut_ptr().cast(),
        iov_len: chunk.len(),
    };
    // SAFETY: msghdr is plain old data.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    let mut cmsg_buf = vec![0u64; (64 + MAX_FDS_PER_CHUNK * 4).div_ceil(8)];
    msg.msg_control = cmsg_buf.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(cmsg_buf.as_slice()) as _;
    let n = loop {
        // SAFETY: msg points at valid buffers.
        let n = unsafe { libc::recvmsg(sock.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(e);
        }
        break n as usize;
    };
    // SAFETY: walking the control buffer the kernel just filled.
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / 4;
                for i in 0..count {
                    fds.push_back(OwnedFd::from_raw_fd(data.add(i).read_unaligned()));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    if msg.msg_flags & libc::MSG_CTRUNC != 0 {
        return Err(invalid("descriptors truncated"));
    }
    buf.extend_from_slice(&chunk[..n]);
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unescapes_addresses() {
        assert_eq!(unescape("/run/user/1000/bus"), b"/run/user/1000/bus");
        assert_eq!(unescape("/tmp/a%20b"), b"/tmp/a b");
        assert_eq!(unescape("/tmp/%zz"), b"/tmp/%zz");
        assert_eq!(unescape("x%2"), b"x%2");
        assert_eq!(unescape("/tmp/%aé"), "/tmp/%aé".as_bytes());
        assert_eq!(unescape("%+1"), b"%+1");
    }
}
