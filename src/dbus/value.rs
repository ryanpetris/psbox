//! D-Bus values and their little-endian wire encoding.

use std::fmt;

/// A decoded D-Bus value. Arrays and dictionaries keep their element
/// signatures so empty containers re-encode correctly.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Byte(u8),
    Bool(bool),
    I16(i16),
    U16(u16),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    Double(f64),
    Str(String),
    Path(String),
    Sig(String),
    /// Index into the message's descriptor list.
    Fd(u32),
    Array(String, Vec<Value>),
    Dict(String, String, Vec<(Value, Value)>),
    Struct(Vec<Value>),
    Variant(Box<Value>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "D-Bus decode: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

fn err<T>(msg: impl Into<String>) -> Result<T, DecodeError> {
    Err(DecodeError(msg.into()))
}

const MAX_DEPTH: usize = 64;

impl Value {
    /// The value's single complete type signature.
    pub fn signature(&self) -> String {
        match self {
            Value::Byte(_) => "y".into(),
            Value::Bool(_) => "b".into(),
            Value::I16(_) => "n".into(),
            Value::U16(_) => "q".into(),
            Value::I32(_) => "i".into(),
            Value::U32(_) => "u".into(),
            Value::I64(_) => "x".into(),
            Value::U64(_) => "t".into(),
            Value::Double(_) => "d".into(),
            Value::Str(_) => "s".into(),
            Value::Path(_) => "o".into(),
            Value::Sig(_) => "g".into(),
            Value::Fd(_) => "h".into(),
            Value::Array(elem, _) => format!("a{elem}"),
            Value::Dict(k, v, _) => format!("a{{{k}{v}}}"),
            Value::Struct(fields) => format!("({})", fields.iter().map(Value::signature).collect::<String>()),
            Value::Variant(_) => "v".into(),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) | Value::Path(s) | Value::Sig(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Value::U32(v) => Some(*v),
            _ => None,
        }
    }

    /// Looks up `key` in an `a{sv}` dictionary, unwrapping the variant.
    pub fn dict_get(&self, key: &str) -> Option<&Value> {
        let Value::Dict(_, _, entries) = self else { return None };
        entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| match v {
                Value::Variant(inner) => inner.as_ref(),
                other => other,
            })
    }

    /// Sets `key` in an `a{sv}` dictionary to a variant holding `value`.
    pub fn dict_set(&mut self, key: &str, value: Value) {
        let Value::Dict(_, _, entries) = self else { return };
        let v = Value::Variant(Box::new(value));
        match entries.iter_mut().find(|(k, _)| k.as_str() == Some(key)) {
            Some(entry) => entry.1 = v,
            None => entries.push((Value::Str(key.into()), v)),
        }
    }

    /// Removes `key` from an `a{sv}` dictionary.
    pub fn dict_remove(&mut self, key: &str) {
        if let Value::Dict(_, _, entries) = self {
            entries.retain(|(k, _)| k.as_str() != Some(key));
        }
    }

    /// Applies `f` to every object path in this value, recursively.
    pub fn map_paths(&mut self, f: &mut dyn FnMut(&str) -> Option<String>) {
        match self {
            Value::Path(p) => {
                if let Some(new) = f(p) {
                    *p = new;
                }
            }
            Value::Array(_, items) | Value::Struct(items) => items.iter_mut().for_each(|v| v.map_paths(f)),
            Value::Dict(_, _, entries) => entries.iter_mut().for_each(|(k, v)| {
                k.map_paths(f);
                v.map_paths(f);
            }),
            Value::Variant(inner) => inner.map_paths(f),
            _ => {}
        }
    }
}

/// Splits a signature into its complete types.
pub fn split_signature(sig: &str) -> Result<Vec<&str>, DecodeError> {
    let mut out = Vec::new();
    let mut rest = sig;
    while !rest.is_empty() {
        let len = complete_type_len(rest, 0)?;
        out.push(&rest[..len]);
        rest = &rest[len..];
    }
    Ok(out)
}

fn complete_type_len(sig: &str, depth: usize) -> Result<usize, DecodeError> {
    if depth > MAX_DEPTH {
        return err("signature too deep");
    }
    let bytes = sig.as_bytes();
    match bytes.first() {
        Some(b'y' | b'b' | b'n' | b'q' | b'i' | b'u' | b'x' | b't' | b'd' | b's' | b'o' | b'g' | b'h' | b'v') => Ok(1),
        Some(b'a') => Ok(1 + complete_type_len(&sig[1..], depth + 1)?),
        Some(b'(') => {
            let mut i = 1;
            while bytes.get(i) != Some(&b')') {
                if i >= bytes.len() {
                    return err("unterminated struct");
                }
                i += complete_type_len(&sig[i..], depth + 1)?;
            }
            if i == 1 {
                return err("empty struct");
            }
            Ok(i + 1)
        }
        Some(b'{') => {
            let k = complete_type_len(&sig[1..], depth + 1)?;
            let v = complete_type_len(&sig[1 + k..], depth + 1)?;
            if bytes.get(1 + k + v) != Some(&b'}') {
                return err("bad dict entry");
            }
            Ok(k + v + 2)
        }
        Some(c) => err(format!("bad signature character {:?}", *c as char)),
        None => err("truncated signature"),
    }
}

fn alignment(sig: &str) -> usize {
    match sig.as_bytes()[0] {
        b'y' | b'g' | b'v' => 1,
        b'n' | b'q' => 2,
        b'x' | b't' | b'd' | b'(' | b'{' => 8,
        _ => 4,
    }
}

/// Reads values from a buffer whose offset 0 is 8-byte aligned.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn at(buf: &'a [u8], pos: usize) -> Self {
        Self { buf, pos }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn align(&mut self, n: usize) -> Result<(), DecodeError> {
        let pad = (n - self.pos % n) % n;
        if self.pos + pad > self.buf.len() {
            return err("truncated padding");
        }
        self.pos += pad;
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.pos + n > self.buf.len() {
            return err("truncated value");
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        self.align(4)?;
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("4 bytes")))
    }

    fn string(&mut self, len: usize) -> Result<String, DecodeError> {
        let s = self.take(len)?;
        if self.take(1)? != [0] {
            return err("string not NUL terminated");
        }
        String::from_utf8(s.to_vec()).map_err(|_| DecodeError("invalid UTF-8".into()))
    }

    /// Reads one value of the single complete type `sig`.
    pub fn read(&mut self, sig: &str) -> Result<Value, DecodeError> {
        self.read_depth(sig, 0)
    }

    fn read_depth(&mut self, sig: &str, depth: usize) -> Result<Value, DecodeError> {
        if depth > MAX_DEPTH {
            return err("value too deep");
        }
        self.align(alignment(sig))?;
        let le = |b: &[u8]| -> [u8; 8] {
            let mut a = [0u8; 8];
            a[..b.len()].copy_from_slice(b);
            a
        };
        Ok(match sig.as_bytes()[0] {
            b'y' => Value::Byte(self.take(1)?[0]),
            b'b' => match self.u32()? {
                0 => Value::Bool(false),
                1 => Value::Bool(true),
                v => return err(format!("bad boolean {v}")),
            },
            b'n' => Value::I16(i16::from_le_bytes(self.take(2)?.try_into().expect("2"))),
            b'q' => Value::U16(u16::from_le_bytes(self.take(2)?.try_into().expect("2"))),
            b'i' => Value::I32(self.u32()? as i32),
            b'u' => Value::U32(self.u32()?),
            b'h' => Value::Fd(self.u32()?),
            b'x' => Value::I64(i64::from_le_bytes(le(self.take(8)?))),
            b't' => Value::U64(u64::from_le_bytes(le(self.take(8)?))),
            b'd' => Value::Double(f64::from_le_bytes(le(self.take(8)?))),
            b's' => {
                let n = self.u32()? as usize;
                Value::Str(self.string(n)?)
            }
            b'o' => {
                let n = self.u32()? as usize;
                Value::Path(self.string(n)?)
            }
            b'g' => {
                let n = self.take(1)?[0] as usize;
                Value::Sig(self.string(n)?)
            }
            b'v' => {
                let n = self.take(1)?[0] as usize;
                let inner = self.string(n)?;
                if split_signature(&inner)?.len() != 1 {
                    return err("variant must hold one complete type");
                }
                Value::Variant(Box::new(self.read_depth(&inner, depth + 1)?))
            }
            b'(' => {
                let mut fields = Vec::new();
                for t in split_signature(&sig[1..sig.len() - 1])? {
                    fields.push(self.read_depth(t, depth + 1)?);
                }
                Value::Struct(fields)
            }
            b'a' => {
                let len = self.u32()? as usize;
                let elem = &sig[1..];
                self.align(alignment(elem))?;
                let end = self.pos + len;
                if end > self.buf.len() {
                    return err("array overruns buffer");
                }
                if elem.starts_with('{') {
                    let inner = &elem[1..elem.len() - 1];
                    let k = &inner[..complete_type_len(inner, depth)?];
                    let v = &inner[k.len()..];
                    let mut entries = Vec::new();
                    while self.pos < end {
                        self.align(8)?;
                        let key = self.read_depth(k, depth + 1)?;
                        let val = self.read_depth(v, depth + 1)?;
                        entries.push((key, val));
                    }
                    if self.pos != end {
                        return err("array element overruns the array");
                    }
                    Value::Dict(k.into(), v.into(), entries)
                } else {
                    let mut items = Vec::new();
                    while self.pos < end {
                        items.push(self.read_depth(elem, depth + 1)?);
                    }
                    if self.pos != end {
                        return err("array element overruns the array");
                    }
                    Value::Array(elem.into(), items)
                }
            }
            c => return err(format!("cannot read type {:?}", c as char)),
        })
    }
}

/// Writes values into a buffer whose offset 0 is 8-byte aligned.
#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn align(&mut self, n: usize) {
        while !self.buf.len().is_multiple_of(n) {
            self.buf.push(0);
        }
    }

    pub fn u32(&mut self, v: u32) {
        self.align(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    fn sig(&mut self, s: &str) {
        self.buf.push(s.len() as u8);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    pub fn write(&mut self, v: &Value) {
        self.align(alignment(&v.signature()));
        match v {
            Value::Byte(b) => self.buf.push(*b),
            Value::Bool(b) => self.u32(*b as u32),
            Value::I16(n) => self.buf.extend_from_slice(&n.to_le_bytes()),
            Value::U16(n) => self.buf.extend_from_slice(&n.to_le_bytes()),
            Value::I32(n) => self.u32(*n as u32),
            Value::U32(n) | Value::Fd(n) => self.u32(*n),
            Value::I64(n) => self.buf.extend_from_slice(&n.to_le_bytes()),
            Value::U64(n) => self.buf.extend_from_slice(&n.to_le_bytes()),
            Value::Double(n) => self.buf.extend_from_slice(&n.to_le_bytes()),
            Value::Str(s) | Value::Path(s) => self.string(s),
            Value::Sig(s) => self.sig(s),
            Value::Variant(inner) => {
                self.sig(&inner.signature());
                self.write(inner);
            }
            Value::Struct(fields) => fields.iter().for_each(|f| self.write(f)),
            Value::Array(elem, items) => {
                self.u32(0);
                let len_at = self.buf.len() - 4;
                self.align(alignment(elem));
                let start = self.buf.len();
                items.iter().for_each(|i| self.write(i));
                let len = (self.buf.len() - start) as u32;
                self.buf[len_at..len_at + 4].copy_from_slice(&len.to_le_bytes());
            }
            Value::Dict(_, _, entries) => {
                self.u32(0);
                let len_at = self.buf.len() - 4;
                self.align(8);
                let start = self.buf.len();
                for (k, val) in entries {
                    self.align(8);
                    self.write(k);
                    self.write(val);
                }
                let len = (self.buf.len() - start) as u32;
                self.buf[len_at..len_at + 4].copy_from_slice(&len.to_le_bytes());
            }
        }
    }
}

/// Decodes a message body with signature `sig`.
pub fn decode_body(sig: &str, body: &[u8]) -> Result<Vec<Value>, DecodeError> {
    let mut r = Reader::new(body);
    let mut out = Vec::new();
    for t in split_signature(sig)? {
        out.push(r.read(t)?);
    }
    if r.pos() != body.len() {
        return err("trailing bytes in body");
    }
    Ok(out)
}

/// Encodes values as a message body, returning the signature and bytes.
pub fn encode_body(values: &[Value]) -> (String, Vec<u8>) {
    let mut w = Writer::default();
    values.iter().for_each(|v| w.write(v));
    (values.iter().map(Value::signature).collect(), w.buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Value> {
        let mut opts = Value::Dict("s".into(), "v".into(), vec![]);
        opts.dict_set("handle_token", Value::Str("t1".into()));
        opts.dict_set("multiple", Value::Bool(true));
        opts.dict_set(
            "filters",
            Value::Array(
                "(sa(us))".into(),
                vec![Value::Struct(vec![
                    Value::Str("Images".into()),
                    Value::Array(
                        "(us)".into(),
                        vec![Value::Struct(vec![Value::U32(0), Value::Str("*.png".into())])],
                    ),
                ])],
            ),
        );
        vec![
            Value::Byte(7),
            Value::Path("/org/freedesktop/portal/desktop/request/1_5/t1".into()),
            Value::U64(u64::MAX),
            Value::Array("s".into(), vec![]),
            Value::Fd(0),
            Value::Double(1.5),
            opts,
        ]
    }

    #[test]
    fn round_trips() {
        let values = sample();
        let (sig, body) = encode_body(&values);
        assert_eq!(sig, "yotashda{sv}");
        assert_eq!(decode_body(&sig, &body).unwrap(), values);
    }

    #[test]
    fn dict_helpers() {
        let mut d = sample().pop().unwrap();
        assert_eq!(d.dict_get("handle_token").and_then(Value::as_str), Some("t1"));
        d.dict_set("multiple", Value::Bool(false));
        assert_eq!(d.dict_get("multiple"), Some(&Value::Bool(false)));
        d.dict_remove("multiple");
        assert_eq!(d.dict_get("multiple"), None);
    }

    #[test]
    fn maps_nested_paths() {
        let mut v = Value::Variant(Box::new(Value::Array("o".into(), vec![Value::Path("/a".into())])));
        v.map_paths(&mut |p| Some(format!("{p}/x")));
        assert_eq!(
            v,
            Value::Variant(Box::new(Value::Array("o".into(), vec![Value::Path("/a/x".into())])))
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert!(split_signature("a").is_err());
        assert!(split_signature("(").is_err());
        assert!(split_signature("{ss}").is_ok());
        assert!(decode_body("s", &[5, 0, 0, 0, b'a']).is_err());
        assert!(decode_body("b", &[2, 0, 0, 0]).is_err());
        // An array of one byte whose only element is four bytes long.
        assert!(decode_body("au", &[1, 0, 0, 0, 7, 0, 0, 0]).is_err());
        let deep = "a".repeat(100) + "y";
        assert!(split_signature(&deep).is_err());
    }
}
