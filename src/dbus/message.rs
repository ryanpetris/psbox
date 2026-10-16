//! D-Bus message headers. Bodies stay as raw bytes until a caller decodes
//! them, so forwarded messages are not re-encoded unless they change.

use super::value::{DecodeError, Reader, Value, Writer, decode_body, encode_body};

pub const METHOD_CALL: u8 = 1;
pub const METHOD_RETURN: u8 = 2;
pub const ERROR: u8 = 3;
pub const SIGNAL: u8 = 4;

pub const FLAG_NO_REPLY_EXPECTED: u8 = 0x1;

const FIELD_PATH: u8 = 1;
const FIELD_INTERFACE: u8 = 2;
const FIELD_MEMBER: u8 = 3;
const FIELD_ERROR_NAME: u8 = 4;
const FIELD_REPLY_SERIAL: u8 = 5;
const FIELD_DESTINATION: u8 = 6;
const FIELD_SENDER: u8 = 7;
const FIELD_SIGNATURE: u8 = 8;
const FIELD_UNIX_FDS: u8 = 9;

/// The largest message accepted (the D-Bus specification limit).
pub const MAX_MESSAGE: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Message {
    pub kind: u8,
    pub flags: u8,
    pub serial: u32,
    pub path: Option<String>,
    pub interface: Option<String>,
    pub member: Option<String>,
    pub error_name: Option<String>,
    pub reply_serial: Option<u32>,
    pub destination: Option<String>,
    pub sender: Option<String>,
    pub signature: String,
    pub unix_fds: u32,
    pub body: Vec<u8>,
}

fn pad8(n: usize) -> usize {
    n.div_ceil(8) * 8
}

/// The total message length, once the fixed 16-byte prefix is available.
pub fn frame_len(prefix: &[u8; 16]) -> Result<usize, DecodeError> {
    if prefix[0] != b'l' {
        return Err(DecodeError("only little-endian messages are supported".into()));
    }
    let body = u32::from_le_bytes(prefix[4..8].try_into().expect("4")) as usize;
    let fields = u32::from_le_bytes(prefix[12..16].try_into().expect("4")) as usize;
    let total = pad8(16 + fields) + body;
    if total > MAX_MESSAGE {
        return Err(DecodeError("message too large".into()));
    }
    Ok(total)
}

impl Message {
    pub fn method_call(dest: &str, path: &str, iface: &str, member: &str, args: &[Value]) -> Self {
        let mut m = Self {
            kind: METHOD_CALL,
            destination: Some(dest.into()),
            path: Some(path.into()),
            interface: Some(iface.into()),
            member: Some(member.into()),
            ..Default::default()
        };
        m.set_body(args);
        m
    }

    pub fn error(to: &Message, name: &str, text: &str) -> Self {
        let mut m = Self {
            kind: ERROR,
            flags: FLAG_NO_REPLY_EXPECTED,
            reply_serial: Some(to.serial),
            destination: to.sender.clone(),
            error_name: Some(name.into()),
            ..Default::default()
        };
        m.set_body(&[Value::Str(text.into())]);
        m
    }

    pub fn expects_reply(&self) -> bool {
        self.kind == METHOD_CALL && self.flags & FLAG_NO_REPLY_EXPECTED == 0
    }

    pub fn args(&self) -> Result<Vec<Value>, DecodeError> {
        decode_body(&self.signature, &self.body)
    }

    pub fn set_body(&mut self, args: &[Value]) {
        let (sig, body) = encode_body(args);
        self.signature = sig;
        self.body = body;
    }

    pub fn is(&self, iface: &str, member: &str) -> bool {
        self.interface.as_deref() == Some(iface) && self.member.as_deref() == Some(member)
    }

    /// Parses one complete message.
    pub fn parse(buf: &[u8]) -> Result<Self, DecodeError> {
        let prefix: &[u8; 16] = buf
            .get(..16)
            .and_then(|b| b.try_into().ok())
            .ok_or(DecodeError("short header".into()))?;
        let total = frame_len(prefix)?;
        if buf.len() != total {
            return Err(DecodeError("length mismatch".into()));
        }
        if buf[3] != 1 {
            return Err(DecodeError("unsupported protocol version".into()));
        }
        let mut m = Message {
            kind: buf[1],
            flags: buf[2],
            serial: u32::from_le_bytes(buf[8..12].try_into().expect("4")),
            ..Default::default()
        };
        let mut r = Reader::at(buf, 12);
        let Value::Array(_, fields) = r.read("a(yv)")? else {
            unreachable!()
        };
        for f in fields {
            let Value::Struct(mut kv) = f else { continue };
            let (Some(Value::Variant(v)), Some(Value::Byte(code))) = (kv.pop(), kv.pop()) else {
                continue;
            };
            match (code, *v) {
                (FIELD_PATH, Value::Path(s)) => m.path = Some(s),
                (FIELD_INTERFACE, Value::Str(s)) => m.interface = Some(s),
                (FIELD_MEMBER, Value::Str(s)) => m.member = Some(s),
                (FIELD_ERROR_NAME, Value::Str(s)) => m.error_name = Some(s),
                (FIELD_REPLY_SERIAL, Value::U32(n)) => m.reply_serial = Some(n),
                (FIELD_DESTINATION, Value::Str(s)) => m.destination = Some(s),
                (FIELD_SENDER, Value::Str(s)) => m.sender = Some(s),
                (FIELD_SIGNATURE, Value::Sig(s)) => m.signature = s,
                (FIELD_UNIX_FDS, Value::U32(n)) => m.unix_fds = n,
                _ => {}
            }
        }
        // The body starts after the header fields the prefix declares, which
        // frame_len already counted in the total.
        let fields_end = 16 + u32::from_le_bytes(buf[12..16].try_into().expect("4")) as usize;
        if r.pos() != fields_end {
            return Err(DecodeError("header fields length mismatch".into()));
        }
        m.body = buf[pad8(fields_end)..].to_vec();
        Ok(m)
    }

    /// Encodes the message with `serial`. The sender field is never
    /// written; the bus sets it.
    pub fn encode(&self, serial: u32) -> Vec<u8> {
        let mut fields = Vec::new();
        let mut push =
            |code: u8, v: Value| fields.push(Value::Struct(vec![Value::Byte(code), Value::Variant(Box::new(v))]));
        if let Some(p) = &self.path {
            push(FIELD_PATH, Value::Path(p.clone()));
        }
        if let Some(s) = &self.interface {
            push(FIELD_INTERFACE, Value::Str(s.clone()));
        }
        if let Some(s) = &self.member {
            push(FIELD_MEMBER, Value::Str(s.clone()));
        }
        if let Some(s) = &self.error_name {
            push(FIELD_ERROR_NAME, Value::Str(s.clone()));
        }
        if let Some(n) = self.reply_serial {
            push(FIELD_REPLY_SERIAL, Value::U32(n));
        }
        if let Some(s) = &self.destination {
            push(FIELD_DESTINATION, Value::Str(s.clone()));
        }
        if !self.signature.is_empty() {
            push(FIELD_SIGNATURE, Value::Sig(self.signature.clone()));
        }
        if self.unix_fds > 0 {
            push(FIELD_UNIX_FDS, Value::U32(self.unix_fds));
        }
        let mut w = Writer::default();
        w.buf.extend_from_slice(&[b'l', self.kind, self.flags, 1]);
        w.u32(self.body.len() as u32);
        w.u32(serial);
        w.write(&Value::Array("(yv)".into(), fields));
        w.align(8);
        w.buf.extend_from_slice(&self.body);
        w.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_fields_cannot_run_into_the_body() {
        // Found by the dbus_message fuzz target: the last header field runs
        // past the declared field length, which once put the body start
        // beyond the end of the message.
        let msg = [
            0x6c, 0x2, 0x1, 0x1, 0x4, 0x0, 0x0, 0x0, 0x0, 0x0, 0x1, 0x1, 0x4, 0x0, 0x0, 0x0, 0x1, 0x1, 0x67, 0x0, 0x4,
            0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
        ];
        assert!(Message::parse(&msg).is_err());
    }

    #[test]
    fn round_trips() {
        let mut m = Message::method_call(
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
            "ReadOne",
            &[
                Value::Str("org.freedesktop.appearance".into()),
                Value::Str("color-scheme".into()),
            ],
        );
        m.unix_fds = 1;
        let bytes = m.encode(42);
        let prefix: [u8; 16] = bytes[..16].try_into().unwrap();
        assert_eq!(frame_len(&prefix).unwrap(), bytes.len());
        let back = Message::parse(&bytes).unwrap();
        assert_eq!(back.serial, 42);
        assert_eq!(back.signature, "ss");
        assert_eq!(back.unix_fds, 1);
        assert_eq!(back.args().unwrap()[1], Value::Str("color-scheme".into()));
        assert!(back.is("org.freedesktop.portal.Settings", "ReadOne"));
        assert!(back.expects_reply());
    }

    #[test]
    fn replies_target_the_caller() {
        let mut call = Message::method_call("d", "/p", "i", "m", &[]);
        call.serial = 9;
        call.sender = Some(":1.5".into());
        let r = Message::error(&call, "org.freedesktop.DBus.Error.AccessDenied", "no");
        assert_eq!(r.reply_serial, Some(9));
        assert_eq!(r.destination.as_deref(), Some(":1.5"));
        let back = Message::parse(&r.encode(1)).unwrap();
        assert_eq!(
            back.error_name.as_deref(),
            Some("org.freedesktop.DBus.Error.AccessDenied")
        );
    }
}
