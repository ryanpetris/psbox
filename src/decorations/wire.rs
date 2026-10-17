//! The Wayland wire format on the bridge's side of the proxy: requests are
//! parsed and events written by the protocol's own message signatures, and
//! descriptors travel alongside on the socket.

use std::collections::VecDeque;
use std::ffi::CString;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use anyhow::{Context, Result, bail};
use wayland_backend::protocol::{AllowNull, Argument, ArgumentType};

use crate::sys::fd::{STREAM_MAX_FDS, recv_stream, send_stream};

/// Ids from here up are created by the server side.
pub const SERVER_ID_START: u32 = 0xff00_0000;

/// A message's header: the sender's id, then its size and opcode.
const HEADER: usize = 8;

pub type Arg = Argument<u32, OwnedFd>;

/// One request from the bridge, or one event for it.
#[derive(Debug)]
pub struct Message {
    pub sender: u32,
    pub opcode: u16,
    pub args: Vec<Arg>,
}

/// Parses the request at the start of `buf` once all of it has arrived,
/// taking its descriptors from `fds`. `signature` gives the argument types
/// of a sender's request, or `None` for an unknown object or opcode.
/// Returns the message and its size in bytes.
pub fn parse<'a>(
    buf: &[u8],
    fds: &mut VecDeque<OwnedFd>,
    signature: impl FnOnce(u32, u16) -> Option<&'a [ArgumentType]>,
) -> Result<Option<(Message, usize)>> {
    if buf.len() < HEADER {
        return Ok(None);
    }
    let sender = u32::from_ne_bytes(buf[0..4].try_into().expect("four bytes"));
    let word = u32::from_ne_bytes(buf[4..8].try_into().expect("four bytes"));
    let (size, opcode) = ((word >> 16) as usize, (word & 0xffff) as u16);
    if size < HEADER || !size.is_multiple_of(4) {
        bail!("malformed message header (size {size})");
    }
    if buf.len() < size {
        return Ok(None);
    }
    let sig = signature(sender, opcode).with_context(|| format!("unknown request {opcode} on object {sender}"))?;
    let wanted = sig.iter().filter(|t| matches!(t, ArgumentType::Fd)).count();
    if wanted > fds.len() {
        bail!("request {opcode} on object {sender} is missing descriptors");
    }

    let mut body = &buf[HEADER..size];
    let mut args = Vec::with_capacity(sig.len());
    for ty in sig {
        args.push(match ty {
            ArgumentType::Int => Argument::Int(take_u32(&mut body)? as i32),
            ArgumentType::Uint => Argument::Uint(take_u32(&mut body)?),
            ArgumentType::Fixed => Argument::Fixed(take_u32(&mut body)? as i32),
            ArgumentType::Object(_) => Argument::Object(take_u32(&mut body)?),
            ArgumentType::NewId => Argument::NewId(take_u32(&mut body)?),
            ArgumentType::Str(null) => {
                let bytes = take_array(&mut body)?;
                match bytes.split_last() {
                    None if *null == AllowNull::Yes => Argument::Str(None),
                    Some((0, text)) => Argument::Str(Some(Box::new(
                        CString::new(text).context("string with a NUL byte inside")?,
                    ))),
                    _ => bail!("malformed string in request {opcode} on object {sender}"),
                }
            }
            ArgumentType::Array => Argument::Array(Box::new(take_array(&mut body)?.to_vec())),
            ArgumentType::Fd => Argument::Fd(fds.pop_front().expect("counted above")),
        });
    }
    if !body.is_empty() {
        bail!("request {opcode} on object {sender} has {} bytes left over", body.len());
    }
    Ok(Some((Message { sender, opcode, args }, size)))
}

fn take_u32(body: &mut &[u8]) -> Result<u32> {
    let Some((head, rest)) = body.split_first_chunk::<4>() else {
        bail!("message ends early");
    };
    *body = rest;
    Ok(u32::from_ne_bytes(*head))
}

/// A length-prefixed run of bytes, padded to four.
fn take_array<'a>(body: &mut &'a [u8]) -> Result<&'a [u8]> {
    let len = take_u32(body)? as usize;
    let padded = len.checked_next_multiple_of(4).context("array too long")?;
    if body.len() < padded {
        bail!("message ends early");
    }
    let bytes = &body[..len];
    *body = &body[padded..];
    Ok(bytes)
}

/// Appends `msg` to `out`, moving its descriptors to `fds`.
pub fn write(msg: Message, out: &mut Vec<u8>, fds: &mut Vec<OwnedFd>) -> Result<()> {
    let start = out.len();
    out.extend_from_slice(&[0; HEADER]);
    let put_array = |out: &mut Vec<u8>, bytes: &[u8]| {
        out.extend_from_slice(&(bytes.len() as u32).to_ne_bytes());
        out.extend_from_slice(bytes);
        out.resize(out.len().next_multiple_of(4), 0);
    };
    for arg in msg.args {
        match arg {
            Argument::Int(v) | Argument::Fixed(v) => out.extend_from_slice(&v.to_ne_bytes()),
            Argument::Uint(v) | Argument::Object(v) | Argument::NewId(v) => out.extend_from_slice(&v.to_ne_bytes()),
            Argument::Str(None) => out.extend_from_slice(&0u32.to_ne_bytes()),
            Argument::Str(Some(s)) => put_array(out, s.as_bytes_with_nul()),
            Argument::Array(a) => put_array(out, &a),
            Argument::Fd(fd) => fds.push(fd),
        }
    }
    let size = out.len() - start;
    if size > usize::from(u16::MAX) {
        out.truncate(start);
        bail!(
            "event {} on object {} is too large ({size} bytes)",
            msg.opcode,
            msg.sender
        );
    }
    out[start..start + 4].copy_from_slice(&msg.sender.to_ne_bytes());
    out[start + 4..start + HEADER].copy_from_slice(&((size as u32) << 16 | u32::from(msg.opcode)).to_ne_bytes());
    Ok(())
}

/// The proxy's end of the bridge's connection, nonblocking, with what has
/// been read but not parsed and what is waiting to be written.
pub struct Socket {
    fd: OwnedFd,
    incoming: Vec<u8>,
    incoming_fds: VecDeque<OwnedFd>,
    outgoing: Vec<u8>,
    /// Descriptors waiting to go out, each with the stream offset of the
    /// message that carries it.
    outgoing_fds: VecDeque<(u64, OwnedFd)>,
    /// Bytes written to the socket so far.
    written: u64,
}

impl Socket {
    pub fn new(fd: OwnedFd) -> Self {
        Self {
            fd,
            incoming: Vec::new(),
            incoming_fds: VecDeque::new(),
            outgoing: Vec::new(),
            outgoing_fds: VecDeque::new(),
            written: 0,
        }
    }

    /// Reads everything waiting. Returns `false` at the end of the stream.
    pub fn fill(&mut self) -> io::Result<bool> {
        let mut buf = [0u8; 4096];
        loop {
            let mut fds = Vec::new();
            match recv_stream(self.fd.as_fd(), &mut buf, &mut fds) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    self.incoming.extend_from_slice(&buf[..n]);
                    self.incoming_fds.extend(fds);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(true),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// The next complete request read so far.
    pub fn next<'a>(
        &mut self,
        signature: impl FnOnce(u32, u16) -> Option<&'a [ArgumentType]>,
    ) -> Result<Option<Message>> {
        let Some((msg, size)) = parse(&self.incoming, &mut self.incoming_fds, signature)? else {
            return Ok(None);
        };
        self.incoming.drain(..size);
        Ok(Some(msg))
    }

    /// Queues an event.
    pub fn send(&mut self, msg: Message) -> Result<()> {
        let at = self.written + self.outgoing.len() as u64;
        let mut fds = Vec::new();
        write(msg, &mut self.outgoing, &mut fds)?;
        self.outgoing_fds.extend(fds.into_iter().map(|fd| (at, fd)));
        Ok(())
    }

    pub fn wants_write(&self) -> bool {
        !self.outgoing.is_empty()
    }

    /// Writes what the socket takes. A message's descriptors go out no later
    /// than its first byte, and at most [`STREAM_MAX_FDS`] go with one write.
    pub fn flush(&mut self) -> io::Result<()> {
        while !self.outgoing.is_empty() {
            let mut n = self.outgoing_fds.len().min(STREAM_MAX_FDS);
            let mut limit = self.outgoing.len();
            if n < self.outgoing_fds.len() {
                // Stop before the first message whose descriptors are left
                // for the next write, keeping each message's together.
                while n > 0 && self.outgoing_fds[n].0 == self.outgoing_fds[n - 1].0 {
                    n -= 1;
                }
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "too many descriptors in one message",
                    ));
                }
                limit = (self.outgoing_fds[n].0 - self.written) as usize;
            }
            let fds: Vec<BorrowedFd<'_>> = self.outgoing_fds.iter().take(n).map(|(_, fd)| fd.as_fd()).collect();
            match send_stream(self.fd.as_fd(), &self.outgoing[..limit], &fds) {
                Ok(sent) => {
                    self.outgoing.drain(..sent);
                    self.written += sent as u64;
                    self.outgoing_fds.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

impl AsFd for Socket {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIG: &[ArgumentType] = &[
        ArgumentType::Uint,
        ArgumentType::Str(AllowNull::No),
        ArgumentType::Uint,
        ArgumentType::NewId,
    ];

    fn message(args: Vec<Arg>) -> Vec<u8> {
        let mut out = Vec::new();
        write(
            Message {
                sender: 2,
                opcode: 0,
                args,
            },
            &mut out,
            &mut Vec::new(),
        )
        .unwrap();
        out
    }

    #[test]
    fn requests_round_trip_and_wait_for_all_their_bytes() {
        let bytes = message(vec![
            Argument::Uint(7),
            Argument::Str(Some(Box::new(CString::new("wl_seat").unwrap()))),
            Argument::Uint(9),
            Argument::NewId(12),
        ]);
        // 8 header, 4 name, 4 + 8 string, 4 version, 4 id.
        assert_eq!(bytes.len(), 32);
        let mut fds = VecDeque::new();
        for partial in [&bytes[..4], &bytes[..31]] {
            assert!(parse(partial, &mut fds, |_, _| Some(SIG)).unwrap().is_none());
        }
        let (msg, size) = parse(&bytes, &mut fds, |_, _| Some(SIG)).unwrap().unwrap();
        assert_eq!((msg.sender, msg.opcode, size), (2, 0, 32));
        assert!(matches!(&msg.args[1], Argument::Str(Some(s)) if s.as_bytes() == b"wl_seat"));
        assert!(matches!(msg.args[3], Argument::NewId(12)));
    }

    #[test]
    fn malformed_requests_are_errors() {
        let good = message(vec![
            Argument::Uint(7),
            Argument::Str(Some(Box::new(CString::new("ab").unwrap()))),
            Argument::Uint(9),
            Argument::NewId(12),
        ]);
        let mut fds = VecDeque::new();
        assert!(parse(&good, &mut fds, |_, _| None).is_err());
        // No NUL at the end of the string.
        let mut bad = good.clone();
        bad[18] = b'c';
        assert!(parse(&bad, &mut fds, |_, _| Some(SIG)).is_err());
        // A null string where none is allowed.
        let null = message(vec![
            Argument::Uint(7),
            Argument::Str(None),
            Argument::Uint(9),
            Argument::NewId(12),
        ]);
        assert!(parse(&null, &mut fds, |_, _| Some(SIG)).is_err());
        // Bytes left over, and a size that is not a multiple of four.
        assert!(parse(&good, &mut fds, |_, _| Some(&SIG[..3])).is_err());
        let mut odd = good.clone();
        odd[6] = 31;
        assert!(parse(&odd, &mut fds, |_, _| Some(SIG)).is_err());
        // A descriptor that never came.
        let fd = message(vec![]);
        assert!(parse(&fd, &mut fds, |_, _| Some(&[ArgumentType::Fd])).is_err());
    }
}
