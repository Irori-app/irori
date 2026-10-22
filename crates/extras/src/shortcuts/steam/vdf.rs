//! Valve's binary KeyValues ("binary VDF") format, as used by Steam's
//! `userdata/<id>/config/shortcuts.vdf`.
//!
//! A document is a sequence of `type:u8, key:cstring, value` items closed by
//! `0x08`. Maps nest. Values keep their wire type, and key order is preserved,
//! so a parse/write round trip is byte-identical.

use anyhow::{Result, anyhow, bail};

const T_MAP: u8 = 0x00;
const T_STRING: u8 = 0x01;
const T_INT32: u8 = 0x02;
const T_FLOAT32: u8 = 0x03;
const T_POINTER: u8 = 0x04;
const T_WSTRING: u8 = 0x05;
const T_COLOR: u8 = 0x06;
const T_UINT64: u8 = 0x07;
const T_END: u8 = 0x08;
const T_INT64: u8 = 0x0A;

/// One value. Variants map 1:1 to wire types.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Map(Map),
    String(String),
    Int32(i32),
    Float32(f32),
    Pointer(u32),
    /// UTF-16 code units, without the terminator.
    WideString(Vec<u16>),
    Color(u32),
    UInt64(u64),
    Int64(i64),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i32(&self) -> Option<i32> {
        match self {
            Value::Int32(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&Map> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    pub fn as_map_mut(&mut self) -> Option<&mut Map> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }
}

/// An ordered key/value map. Keys may repeat on the wire; lookups return the
/// first match.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Map(pub Vec<(String, Value)>);

impl Map {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an item (builder style).
    pub fn with(mut self, key: impl Into<String>, value: Value) -> Self {
        self.0.push((key.into(), value));
        self
    }

    pub fn push(&mut self, key: impl Into<String>, value: Value) {
        self.0.push((key.into(), value));
    }

    /// First value whose key equals `key`, ignoring ASCII case.
    pub fn get_ci(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    pub fn get_ci_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.0
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    }

    /// String value of `key` (case-insensitive), or `""`.
    pub fn str_ci(&self, key: &str) -> &str {
        self.get_ci(key).and_then(Value::as_str).unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// Parse a whole document (the root map body). A missing final `0x08` at the
/// end of the input is tolerated; bytes after the root terminator are ignored.
pub fn parse(bytes: &[u8]) -> Result<Map> {
    let mut r = Reader { buf: bytes, pos: 0 };
    r.map_body(true)
}

/// Serialize a root map. The output of [`parse`] writes back unchanged.
pub fn write(root: &Map) -> Vec<u8> {
    let mut out = Vec::with_capacity(1024);
    write_map_body(&mut out, root);
    out
}

fn write_map_body(out: &mut Vec<u8>, map: &Map) {
    for (key, value) in &map.0 {
        let ty = match value {
            Value::Map(_) => T_MAP,
            Value::String(_) => T_STRING,
            Value::Int32(_) => T_INT32,
            Value::Float32(_) => T_FLOAT32,
            Value::Pointer(_) => T_POINTER,
            Value::WideString(_) => T_WSTRING,
            Value::Color(_) => T_COLOR,
            Value::UInt64(_) => T_UINT64,
            Value::Int64(_) => T_INT64,
        };
        out.push(ty);
        out.extend_from_slice(key.as_bytes());
        out.push(0);
        match value {
            Value::Map(m) => write_map_body(out, m),
            Value::String(s) => {
                out.extend_from_slice(s.as_bytes());
                out.push(0);
            }
            Value::Int32(v) => out.extend_from_slice(&v.to_le_bytes()),
            Value::Float32(v) => out.extend_from_slice(&v.to_le_bytes()),
            Value::Pointer(v) | Value::Color(v) => out.extend_from_slice(&v.to_le_bytes()),
            Value::WideString(units) => {
                for u in units {
                    out.extend_from_slice(&u.to_le_bytes());
                }
                out.extend_from_slice(&[0, 0]);
            }
            Value::UInt64(v) => out.extend_from_slice(&v.to_le_bytes()),
            Value::Int64(v) => out.extend_from_slice(&v.to_le_bytes()),
        }
    }
    out.push(T_END);
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8> {
        let b = *self
            .buf
            .get(self.pos)
            .ok_or_else(|| anyhow!("unexpected end of data at byte {}", self.pos))?;
        self.pos += 1;
        Ok(b)
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self.pos + N;
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or_else(|| anyhow!("unexpected end of data at byte {}", self.pos))?;
        self.pos = end;
        let mut arr = [0u8; N];
        arr.copy_from_slice(slice);
        Ok(arr)
    }

    fn cstring(&mut self) -> Result<String> {
        let start = self.pos;
        let len = self.buf[start..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| anyhow!("unterminated string at byte {start}"))?;
        self.pos = start + len + 1;
        String::from_utf8(self.buf[start..start + len].to_vec())
            .map_err(|_| anyhow!("string at byte {start} is not valid UTF-8"))
    }

    fn wstring(&mut self) -> Result<Vec<u16>> {
        let mut units = Vec::new();
        loop {
            let u = u16::from_le_bytes(self.take::<2>()?);
            if u == 0 {
                return Ok(units);
            }
            units.push(u);
        }
    }

    fn map_body(&mut self, root: bool) -> Result<Map> {
        let mut map = Map::new();
        loop {
            if root && self.pos >= self.buf.len() {
                return Ok(map);
            }
            let ty = self.byte()?;
            if ty == T_END {
                return Ok(map);
            }
            let at = self.pos - 1;
            let key = self.cstring()?;
            let value = match ty {
                T_MAP => Value::Map(self.map_body(false)?),
                T_STRING => Value::String(self.cstring()?),
                T_INT32 => Value::Int32(i32::from_le_bytes(self.take()?)),
                T_FLOAT32 => Value::Float32(f32::from_le_bytes(self.take()?)),
                T_POINTER => Value::Pointer(u32::from_le_bytes(self.take()?)),
                T_WSTRING => Value::WideString(self.wstring()?),
                T_COLOR => Value::Color(u32::from_le_bytes(self.take()?)),
                T_UINT64 => Value::UInt64(u64::from_le_bytes(self.take()?)),
                T_INT64 => Value::Int64(i64::from_le_bytes(self.take()?)),
                other => bail!("unknown value type 0x{other:02x} at byte {at}"),
            };
            map.push(key, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_types_roundtrip() {
        let root = Map::new().with(
            "root",
            Value::Map(
                Map::new()
                    .with("s", Value::String("héllo".into()))
                    .with("i", Value::Int32(-5))
                    .with("f", Value::Float32(1.5))
                    .with("p", Value::Pointer(7))
                    .with("w", Value::WideString("hi".encode_utf16().collect()))
                    .with("c", Value::Color(0xff00ff))
                    .with("u", Value::UInt64(u64::MAX))
                    .with("l", Value::Int64(-1))
                    .with("m", Value::Map(Map::new())),
            ),
        );
        let bytes = write(&root);
        let back = parse(&bytes).unwrap();
        assert_eq!(back, root);
        assert_eq!(write(&back), bytes);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(&[0x01, b'a']).is_err());
        assert!(parse(&[0x09, b'a', 0]).is_err());
        assert!(parse(&[0x02, b'a', 0, 1, 2]).is_err());
        assert!(parse(&[0x00, b'a', 0, 0x01, b'k', 0, b'v']).is_err());
    }

    #[test]
    fn signed_int_decoding() {
        let bytes = [
            0x02, b'a', b'p', b'p', b'i', b'd', 0, 0xa4, 0x6e, 0xac, 0x9e, 0x08,
        ];
        let m = parse(&bytes).unwrap();
        assert_eq!(
            m.get_ci("APPID").and_then(Value::as_i32),
            Some(-1_632_866_652)
        );
    }
}
