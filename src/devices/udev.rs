//! Bounded libudev wire messages and netlink sockets.

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

use super::{Device, valid_dev_name, valid_key, valid_subsystem, valid_tag};
use crate::sys::cvt_int;

const HEADER: usize = 40;
const MAX_IN: usize = 65536;
const MAX_OUT: usize = 8192;
const MAGIC: u32 = 0xfeedcafe;

#[derive(Debug, PartialEq, Eq)]
pub struct Event {
    pub key: (u32, u32),
    /// A failed or processing event can withdraw access, but cannot publish.
    pub failed: bool,
}

pub(super) fn processing_property(key: &[u8]) -> bool {
    matches!(
        key,
        b"ID_PROCESSING"
            | b"UDEV_WORKER_FAILED"
            | b"UDEV_WORKER_ERRNO"
            | b"UDEV_WORKER_EXIT_STATUS"
            | b"UDEV_WORKER_SIGNAL"
    )
}

pub(super) fn valid_devpath(path: &str) -> bool {
    path.starts_with("/devices/")
        && path[1..].split('/').all(|p| !p.is_empty() && p != "." && p != "..")
        && !path.bytes().any(|c| c == 0 || c.is_ascii_whitespace())
}

/// The kernel restricts host group 2 senders with CAP_NET_ADMIN in the
/// initial user namespace. Credentials must exist, but their uid can be
/// unmapped after the supervisor enters its user namespace.
#[derive(Debug, PartialEq, Eq)]
pub enum Received {
    Event(Event),
    Ignored,
    Invalid,
}

fn parse(buf: &[u8], group: u32, port: u32, credentials: bool, flags: i32) -> Received {
    if group != 2 || port == 0 || !credentials {
        return Received::Ignored;
    }
    if flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
        return Received::Invalid;
    }
    parse_packet(buf).unwrap_or(Received::Invalid)
}

fn parse_packet(buf: &[u8]) -> Option<Received> {
    if buf.len() < HEADER || buf.len() > MAX_IN || &buf[..8] != b"libudev\0" || buf[8..12] != MAGIC.to_be_bytes() {
        return None;
    }
    let native = |at| u32::from_ne_bytes(buf[at..at + 4].try_into().expect("header field")) as usize;
    let (header, offset, len) = (native(12), native(16), native(20));
    if header < HEADER || offset < header || offset.checked_add(len)? != buf.len() || len == 0 {
        return None;
    }
    let data = buf.get(offset..)?;
    let fields = data.strip_suffix(&[0])?;
    let mut subsystem = None;
    let mut major = None;
    let mut minor = None;
    let mut name = None;
    let mut malformed = false;
    let mut failed = false;
    for field in fields.split(|&c| c == 0) {
        let mut parts = field.splitn(2, |&c| c == b'=');
        let key = parts.next().expect("split yields at least one field");
        failed |= processing_property(key);
        let Some(value) = parts.next() else {
            malformed = true;
            continue;
        };
        let slot = match key {
            b"SUBSYSTEM" => &mut subsystem,
            b"MAJOR" => &mut major,
            b"MINOR" => &mut minor,
            b"DEVNAME" => &mut name,
            _ => continue,
        };
        if slot.replace(value).is_some() {
            return None;
        }
    }
    if subsystem == Some(b"block") || (major.is_none() && minor.is_none() && name.is_none()) {
        return Some(Received::Ignored);
    }

    let subsystem = std::str::from_utf8(subsystem?).ok()?;
    let name = std::str::from_utf8(name?).ok()?.strip_prefix("/dev/")?;
    if malformed || !valid_subsystem(subsystem) || !valid_dev_name(name) {
        return None;
    }
    let major = std::str::from_utf8(major?).ok()?.parse().ok()?;
    let minor = std::str::from_utf8(minor?).ok()?.parse().ok()?;
    Some(Received::Event(Event {
        key: (major, minor),
        failed,
    }))
}

fn socket(groups: u32) -> io::Result<OwnedFd> {
    // SAFETY: plain socket syscall; ownership transfers immediately.
    let fd = cvt_int(unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_KOBJECT_UEVENT,
        )
    })?;
    // SAFETY: fd was just created.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: sockaddr_nl is plain data and bind reads its initialized fields.
    unsafe {
        let mut addr: libc::sockaddr_nl = std::mem::zeroed();
        addr.nl_family = libc::AF_NETLINK as _;
        addr.nl_groups = groups;
        cvt_int(libc::bind(
            fd.as_raw_fd(),
            (&addr as *const libc::sockaddr_nl).cast(),
            std::mem::size_of_val(&addr) as _,
        ))?;
    }
    Ok(fd)
}

pub struct Watcher(OwnedFd);

impl AsFd for Watcher {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl Watcher {
    pub fn new() -> io::Result<Self> {
        let fd = socket(2)?;
        let yes: libc::c_int = 1;
        // SAFETY: valid socket and integer option.
        cvt_int(unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                (&yes as *const libc::c_int).cast(),
                std::mem::size_of_val(&yes) as _,
            )
        })?;
        Ok(Self(fd))
    }

    /// Reads one bounded datagram and distinguishes ignored traffic from data loss.
    pub fn receive(&self) -> io::Result<Received> {
        let mut bytes = [0u8; MAX_IN];
        let mut control = [0u64; 16];
        // SAFETY: these C structures contain no invalid bit patterns.
        let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        // SAFETY: zeroed msghdr has null pointers until assigned below.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        msg.msg_name = (&mut addr as *mut libc::sockaddr_nl).cast();
        msg.msg_namelen = std::mem::size_of_val(&addr) as _;
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&control) as _;
        // SAFETY: msg refers to live buffers of the declared sizes.
        let n = unsafe { libc::recvmsg(self.0.as_raw_fd(), &mut msg, 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut credentials = false;
        // SAFETY: walk only ancillary records returned by the kernel, checking credential size.
        unsafe {
            let mut c = libc::CMSG_FIRSTHDR(&msg);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET
                    && (*c).cmsg_type == libc::SCM_CREDENTIALS
                    && (*c).cmsg_len as usize == libc::CMSG_LEN(std::mem::size_of::<libc::ucred>() as _) as usize
                {
                    credentials = true;
                }
                c = libc::CMSG_NXTHDR(&msg, c);
            }
        }
        if msg.msg_namelen as usize != std::mem::size_of_val(&addr)
            || addr.nl_family != libc::AF_NETLINK as libc::sa_family_t
        {
            return Ok(Received::Ignored);
        }
        Ok(parse(
            &bytes[..(n as usize).min(bytes.len())],
            addr.nl_groups,
            addr.nl_pid,
            credentials,
            msg.msg_flags,
        ))
    }
}

/// A socket created and bound inside the sandbox's private network namespace.
pub struct Sender(pub OwnedFd);

impl Sender {
    pub fn new() -> io::Result<Self> {
        socket(0).map(Self)
    }

    pub fn send(&self, dev: &Device, action: &str, seq: u64) -> io::Result<()> {
        let bytes = encode(dev, action, seq)?;
        // SAFETY: initialized destination and live byte buffer.
        let n = unsafe {
            let mut addr: libc::sockaddr_nl = std::mem::zeroed();
            addr.nl_family = libc::AF_NETLINK as _;
            addr.nl_groups = 2;
            libc::sendto(
                self.0.as_raw_fd(),
                bytes.as_ptr().cast(),
                bytes.len(),
                libc::MSG_NOSIGNAL,
                (&addr as *const libc::sockaddr_nl).cast(),
                std::mem::size_of_val(&addr) as _,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n as usize != bytes.len() {
            return Err(io::Error::other("short udev notification"));
        }
        Ok(())
    }
}

// MurmurHash2 with seed zero, as specified by the libudev filter format.
fn hash(value: &str) -> u32 {
    const M: u32 = 0x5bd1e995;
    let mut h = value.len() as u32;
    let (chunks, tail) = value.as_bytes().as_chunks::<4>();
    for chunk in chunks {
        let mut k = u32::from_le_bytes(*chunk);
        k = k.wrapping_mul(M);
        k ^= k >> 24;
        h = h.wrapping_mul(M) ^ k.wrapping_mul(M);
    }
    for (i, byte) in tail.iter().enumerate() {
        h ^= u32::from(*byte) << (8 * i);
    }
    if !tail.is_empty() {
        h = h.wrapping_mul(M);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^ (h >> 15)
}

fn bloom(tags: &[String]) -> u64 {
    tags.iter().fold(0, |bits, tag| {
        let h = hash(tag);
        bits | [0, 6, 12, 18]
            .iter()
            .fold(0, |bits, shift| bits | (1u64 << ((h >> shift) & 63)))
    })
}

fn encode(dev: &Device, action: &str, seq: u64) -> io::Result<Vec<u8>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid udev notification");
    if seq == 0
        || !matches!(action, "add" | "remove")
        || !valid_devpath(&dev.devpath)
        || !valid_dev_name(&dev.devname)
        || !valid_subsystem(&dev.subsystem)
    {
        return Err(invalid());
    }
    // Keep public device properties. Identity, filters, and actions come
    // from the normalized device that was actually published.
    let mut props: BTreeMap<String, String> = dev
        .props
        .iter()
        .filter(|(k, _)| {
            !k.starts_with('.')
                && !k.starts_with("UDEV_WORKER_")
                && !matches!(
                    k.as_str(),
                    "ID_PROCESSING" | "USEC_INITIALIZED" | "SYNTH_UUID" | "DEVPATH_OLD"
                )
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (k, v) in [
        ("ACTION", action.into()),
        ("DEVPATH", dev.devpath.clone()),
        ("SUBSYSTEM", dev.subsystem.clone()),
        ("DEVNAME", format!("/dev/{}", dev.devname)),
        ("MAJOR", dev.major.to_string()),
        ("MINOR", dev.minor.to_string()),
        ("SEQNUM", seq.to_string()),
        ("UDEV_DATABASE_VERSION", "1".into()),
    ] {
        props.insert(k.into(), v);
    }
    if let Some(i) = dev.initialized {
        props.insert("USEC_INITIALIZED".into(), i.to_string());
    }
    for (k, tags) in [("TAGS", &dev.tags), ("CURRENT_TAGS", &dev.current_tags)] {
        if tags.iter().any(|t| !valid_tag(t)) {
            return Err(invalid());
        }
        props.insert(
            k.into(),
            if tags.is_empty() {
                String::new()
            } else {
                format!(":{}:", tags.join(":"))
            },
        );
    }
    if dev.links.iter().any(|l| !valid_dev_name(l)) {
        return Err(invalid());
    }
    props.insert(
        "DEVLINKS".into(),
        dev.links
            .iter()
            .map(|l| format!("/dev/{l}"))
            .collect::<Vec<_>>()
            .join(" "),
    );
    let mut out = vec![0u8; HEADER];
    for (k, v) in props {
        if !valid_key(&k) || v.contains('\0') {
            return Err(invalid());
        }
        if out.len() + k.len() + v.len() + 2 > MAX_OUT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "udev notification exceeds 8192 bytes",
            ));
        }
        out.extend_from_slice(k.as_bytes());
        out.push(b'=');
        out.extend_from_slice(v.as_bytes());
        out.push(0);
    }
    out[..8].copy_from_slice(b"libudev\0");
    out[8..12].copy_from_slice(&MAGIC.to_be_bytes());
    out[12..16].copy_from_slice(&(HEADER as u32).to_ne_bytes());
    out[16..20].copy_from_slice(&(HEADER as u32).to_ne_bytes());
    let len = (out.len() - HEADER) as u32;
    out[20..24].copy_from_slice(&len.to_ne_bytes());
    out[24..28].copy_from_slice(&hash(&dev.subsystem).to_be_bytes());
    out[28..32].copy_from_slice(&dev.props.get("DEVTYPE").map_or(0, |v| hash(v)).to_be_bytes());
    out[32..40].copy_from_slice(&bloom(&dev.tags).to_be_bytes());
    Ok(out)
}

#[cfg(fuzzing)]
pub fn fuzz(data: &[u8]) {
    let _ = super::parse_database(data);
    let _ = parse(data, 2, 1, true, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device() -> Device {
        Device {
            major: 13,
            minor: 70,
            subsystem: "input".into(),
            devpath: "/devices/virtual/input/input6/event6".into(),
            initialized: Some(1234),
            devname: "input/event6".into(),
            links: vec!["input/by-id/pad".into()],
            props: BTreeMap::from([("ID_INPUT_JOYSTICK".into(), "1".into())]),
            tags: vec!["seat".into()],
            current_tags: vec!["seat".into()],
            gamepad_hid: false,
        }
    }

    fn packet(payload: &[u8]) -> Vec<u8> {
        let mut bytes = b"libudev\0\xfe\xed\xca\xfe".to_vec();
        bytes.extend_from_slice(&(HEADER as u32).to_ne_bytes());
        bytes.extend_from_slice(&(HEADER as u32).to_ne_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        bytes.resize(HEADER, 0);
        bytes.extend_from_slice(payload);
        bytes
    }

    const IDENTITY: &[u8] = b"SUBSYSTEM=input\0MAJOR=13\0MINOR=70\0DEVNAME=/dev/input/event6\0";

    #[test]
    fn wire_header_and_properties_have_exact_bytes() {
        assert_eq!(hash("abc"), 0x13577c9b);
        let mut d = device();
        d.props.insert("DEVTYPE".into(), "usb_device".into());
        let out = encode(&d, "add", 7).unwrap();
        let payload = b"ACTION=add\0CURRENT_TAGS=:seat:\0DEVLINKS=/dev/input/by-id/pad\0DEVNAME=/dev/input/event6\0DEVPATH=/devices/virtual/input/input6/event6\0DEVTYPE=usb_device\0ID_INPUT_JOYSTICK=1\0MAJOR=13\0MINOR=70\0SEQNUM=7\0SUBSYSTEM=input\0TAGS=:seat:\0UDEV_DATABASE_VERSION=1\0USEC_INITIALIZED=1234\0";
        let mut expected = packet(payload);
        expected[24..32].copy_from_slice(&[0xc1, 0xa2, 0x84, 0x70, 0x27, 0xf8, 0xf5, 0x0c]);
        expected[32..40].copy_from_slice(&0x0208000000400001u64.to_be_bytes());
        assert_eq!(out, expected);
    }

    #[test]
    fn notifications_keep_public_metadata_and_use_generated_identity() {
        let mut d = device();
        d.tags.insert(0, "old".into());
        for (key, value) in [
            ("WL_SEAT", "game"),
            ("PAD-PROFILE", "usb"),
            ("LIBINPUT_CALIBRATION_MATRIX", "1 0 0 0 1 0"),
            (".INPUT_CLASS", "joystick"),
            ("UDEV_WORKER_FAILED", "1"),
            ("ID_PROCESSING", "1"),
            ("DEVNAME", "/dev/wrong"),
            ("ACTION", "change"),
            ("SEQNUM", "999"),
            ("USEC_INITIALIZED", "999"),
        ] {
            d.props.insert(key.into(), value.into());
        }
        let out = encode(&d, "remove", 8).unwrap();
        let props: Vec<_> = std::str::from_utf8(&out[HEADER..])
            .unwrap()
            .split_terminator('\0')
            .collect();
        for expected in [
            "WL_SEAT=game",
            "PAD-PROFILE=usb",
            "LIBINPUT_CALIBRATION_MATRIX=1 0 0 0 1 0",
            "CURRENT_TAGS=:seat:",
            "TAGS=:old:seat:",
            "UDEV_DATABASE_VERSION=1",
            "USEC_INITIALIZED=1234",
            "DEVNAME=/dev/input/event6",
            "ACTION=remove",
            "SEQNUM=8",
        ] {
            assert!(props.contains(&expected), "{expected}");
        }
        for prefix in [".INPUT_CLASS=", "ID_PROCESSING=", "UDEV_WORKER_"] {
            assert!(!props.iter().any(|p| p.starts_with(prefix)), "{prefix}");
        }
        d.props.insert("ID_LARGE".into(), "x".repeat(MAX_OUT));
        assert_eq!(encode(&d, "add", 9).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn checks_netlink_origin_and_framing() {
        let good = packet(IDENTITY);
        for (group, port, credentials) in [
            (1, 42, true),
            (0, 42, true),
            (3, 42, true),
            (2, 0, true),
            (2, 42, false),
        ] {
            assert_eq!(
                parse(&good, group, port, credentials, libc::MSG_TRUNC),
                Received::Ignored
            );
        }
        for flags in [libc::MSG_TRUNC, libc::MSG_CTRUNC] {
            assert_eq!(parse(&good, 2, 42, true, flags), Received::Invalid);
        }
        for n in 0..good.len() {
            assert_eq!(parse(&good[..n], 2, 42, true, 0), Received::Invalid);
        }
        for at in [0, 7, 8, 12, 16, 20] {
            let mut bad = good.clone();
            bad[at] ^= 0xff;
            assert_eq!(parse(&bad, 2, 42, true, 0), Received::Invalid);
        }
        assert_eq!(parse(&packet(&vec![0; MAX_IN]), 2, 42, true, 0), Received::Invalid);
    }

    #[test]
    fn character_hint_keeps_only_the_key_and_failure_markers() {
        let read = |payload: &[u8]| parse(&packet(payload), 2, u32::MAX, true, 0);
        let hint = |failed| Received::Event(Event { key: (13, 70), failed });
        for suffix in [
            b"ACTION=add\0SEQNUM=bad\0DEVLINKS=invalid\0STRANGE=\xff\0".as_slice(),
            b"ACTION=remove\0",
            b"ACTION=bind\0",
        ] {
            assert_eq!(read(&[IDENTITY, suffix].concat()), hint(false));
        }
        for marker in [
            b"ID_PROCESSING=1\0".as_slice(),
            b"UDEV_WORKER_FAILED=1\0",
            b"UDEV_WORKER_ERRNO=5\0",
            b"UDEV_WORKER_EXIT_STATUS=1\0",
            b"UDEV_WORKER_SIGNAL=9\0",
        ] {
            assert_eq!(read(&[IDENTITY, marker].concat()), hint(true));
        }
        for suffix in [
            b"MAJOR=1\0".as_slice(),
            b"SUBSYSTEM=block\0",
            b"unfinished",
            b"NO_EQUAL\0",
        ] {
            assert_eq!(read(&[IDENTITY, suffix].concat()), Received::Invalid);
        }
        for payload in [
            b"SUBSYSTEM=input\0MAJOR=13\0".as_slice(),
            b"SUBSYSTEM=input\0MAJOR=4294967296\0MINOR=70\0DEVNAME=/dev/input/event6\0",
            b"SUBSYSTEM=input\0MAJOR=13\0MINOR=70\0DEVNAME=/dev/../input/event6\0",
        ] {
            assert_eq!(read(payload), Received::Invalid);
        }
        for payload in [
            b"SUBSYSTEM=block\0MAJOR=8\0BOGUS\0".as_slice(),
            b"SUBSYSTEM=usb\0ACTION=change\0BOGUS\0",
        ] {
            assert_eq!(read(payload), Received::Ignored);
        }
    }
}
