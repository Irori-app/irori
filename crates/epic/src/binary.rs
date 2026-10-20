//! Little-endian readers and writers for Epic's binary formats, with bounds
//! checks on everything (manifests and chunks come from the network).

use anyhow::{Result, bail};

/// A cursor over untrusted bytes.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.data.len() {
            bail!("unexpected end of data");
        }
        self.pos = pos;
        Ok(())
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|end| *end <= self.data.len());
        let Some(end) = end else {
            bail!("unexpected end of data");
        };
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub fn rest(&mut self) -> &'a [u8] {
        let out = &self.data[self.pos..];
        self.pos = self.data.len();
        out
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.bytes(N)?);
        Ok(out)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    /// A count of elements that each take at least `min_size` bytes, checked
    /// against the remaining data so corrupt input cannot request huge
    /// allocations.
    pub fn count(&mut self, min_size: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        if n.saturating_mul(min_size.max(1)) > self.remaining() {
            bail!("corrupt element count {n}");
        }
        Ok(n)
    }

    /// Unreal `FString`: i32 length including the terminator; negative means
    /// UTF-16 (length in code units).
    pub fn fstring(&mut self) -> Result<String> {
        let len = self.i32()?;
        match len {
            0 => Ok(String::new()),
            n if n > 0 => {
                let raw = self.bytes(n as usize)?;
                let text = raw.strip_suffix(&[0]).unwrap_or(raw);
                Ok(String::from_utf8_lossy(text).into_owned())
            }
            n => {
                let units = (n as i64).unsigned_abs() as usize;
                let raw = self.bytes(units.saturating_mul(2))?;
                let words: Vec<u16> = raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .collect();
                let words = words.strip_suffix(&[0]).unwrap_or(&words);
                Ok(String::from_utf16_lossy(words))
            }
        }
    }
}

/// Little-endian writer.
#[derive(Default)]
pub(crate) struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }

    pub fn fstring(&mut self, s: &str) {
        if s.is_empty() {
            self.i32(0);
        } else if s.is_ascii() {
            self.i32(s.len() as i32 + 1);
            self.bytes(s.as_bytes());
            self.u8(0);
        } else {
            let units: Vec<u16> = s.encode_utf16().collect();
            self.i32(-(units.len() as i32 + 1));
            for u in units {
                self.buf.extend_from_slice(&u.to_le_bytes());
            }
            self.buf.extend_from_slice(&[0, 0]);
        }
    }

    /// Reserve a u32 size field; [`Writer::finish_size`] fills it with the
    /// number of bytes written since (including the field).
    pub fn start_size(&mut self) -> usize {
        let at = self.buf.len();
        self.u32(0);
        at
    }

    pub fn finish_size(&mut self, at: usize) {
        let size = (self.buf.len() - at) as u32;
        self.buf[at..at + 4].copy_from_slice(&size.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_round_trip() {
        let mut w = Writer::default();
        for s in ["", "Game.exe", "Spiel/Größe.pak", "日本語"] {
            w.fstring(s);
        }
        w.u32(7);
        let mut r = Reader::new(&w.buf);
        assert_eq!(r.fstring().unwrap(), "");
        assert_eq!(r.fstring().unwrap(), "Game.exe");
        assert_eq!(r.fstring().unwrap(), "Spiel/Größe.pak");
        assert_eq!(r.fstring().unwrap(), "日本語");
        assert_eq!(r.u32().unwrap(), 7);
        assert!(r.u8().is_err());
    }

    #[test]
    fn rejects_truncated_and_huge_input() {
        let mut r = Reader::new(&[5, 0, 0, 0, b'a']);
        assert!(r.fstring().is_err());
        let mut r = Reader::new(&[0xff, 0xff, 0xff, 0x0f, 1, 2]);
        assert!(r.count(4).is_err());
        let mut r = Reader::new(&[0, 0, 0, 0x80]);
        assert!(r.fstring().is_err());
    }
}
