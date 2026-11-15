//! Minecraft Java `FriendlyByteBuf` primitives (big-endian, VarInt lengths).

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodeError {}

pub type DResult<T> = Result<T, DecodeError>;

pub fn err<T>(msg: impl Into<String>) -> DResult<T> {
    Err(DecodeError(msg.into()))
}

/// A player UUID as two halves, matching both the WIT `uuid` record and Java's
/// `UUIDUtil.STREAM_CODEC` (most significant long first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Uuid {
    pub high: u64,
    pub low: u64,
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = self.high;
        let l = self.low;
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            h >> 32,
            (h >> 16) & 0xffff,
            h & 0xffff,
            l >> 48,
            l & 0xffff_ffff_ffff
        )
    }
}

pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn bytes(&mut self, n: usize) -> DResult<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len());
        let Some(end) = end else {
            return err(format!("needed {n} bytes, {} left", self.remaining()));
        };
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    pub fn u8(&mut self) -> DResult<u8> {
        Ok(self.bytes(1)?[0])
    }

    pub fn bool(&mut self) -> DResult<bool> {
        Ok(self.u8()? != 0)
    }

    pub fn i32(&mut self) -> DResult<i32> {
        Ok(i32::from_be_bytes(self.bytes(4)?.try_into().expect("4")))
    }

    pub fn f32(&mut self) -> DResult<f32> {
        Ok(f32::from_be_bytes(self.bytes(4)?.try_into().expect("4")))
    }

    pub fn u64(&mut self) -> DResult<u64> {
        Ok(u64::from_be_bytes(self.bytes(8)?.try_into().expect("8")))
    }

    pub fn uuid(&mut self) -> DResult<Uuid> {
        Ok(Uuid {
            high: self.u64()?,
            low: self.u64()?,
        })
    }

    pub fn var_int(&mut self) -> DResult<i32> {
        let mut value: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            value |= u32::from(b & 0x7F) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(value as i32);
            }
        }
        err("VarInt too big")
    }

    /// `readByteArray(max)`.
    pub fn byte_array(&mut self, max: usize) -> DResult<&'a [u8]> {
        let len = self.var_int()?;
        if len < 0 || len as usize > max {
            return err(format!("byte array length {len} over {max}"));
        }
        self.bytes(len as usize)
    }

    /// `readUtf(max)`: at most `max` UTF-16 units and `max * 3` encoded bytes.
    pub fn utf(&mut self, max: usize) -> DResult<String> {
        let len = self.var_int()?;
        if len < 0 || len as usize > max * 3 {
            return err(format!("string of {len} bytes over the limit {}", max * 3));
        }
        let raw = self.bytes(len as usize)?;
        let s = std::str::from_utf8(raw).map_err(|_| DecodeError("string is not UTF-8".into()))?;
        if s.encode_utf16().count() > max {
            return err(format!("string longer than {max} characters"));
        }
        Ok(s.to_owned())
    }

    pub fn finish(&self) -> DResult<()> {
        if self.remaining() == 0 {
            Ok(())
        } else {
            err(format!("{} unread bytes", self.remaining()))
        }
    }
}

#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(u8::from(v))
    }

    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    pub fn uuid(&mut self, v: Uuid) -> &mut Self {
        self.buf.extend_from_slice(&v.high.to_be_bytes());
        self.buf.extend_from_slice(&v.low.to_be_bytes());
        self
    }

    pub fn var_int(&mut self, v: i32) -> &mut Self {
        let mut v = v as u32;
        loop {
            if v & !0x7F == 0 {
                self.buf.push(v as u8);
                return self;
            }
            self.buf.push((v as u8 & 0x7F) | 0x80);
            v >>= 7;
        }
    }

    pub fn byte_array(&mut self, v: &[u8]) -> &mut Self {
        self.var_int(v.len() as i32);
        self.buf.extend_from_slice(v);
        self
    }

    pub fn utf(&mut self, v: &str) -> &mut Self {
        self.byte_array(v.as_bytes())
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        for v in [0, 1, 127, 128, 255, 25565, 2_097_151, i32::MAX, -1, i32::MIN] {
            let mut w = Writer::new();
            w.var_int(v);
            assert_eq!(Reader::new(&w.buf).var_int().unwrap(), v);
        }
        let mut w = Writer::new();
        w.var_int(300);
        assert_eq!(w.buf, [0xAC, 0x02]);
    }

    #[test]
    fn utf_limits_count_utf16_units() {
        let mut w = Writer::new();
        w.utf("😀😀"); // 4 UTF-16 units, 8 bytes
        assert!(Reader::new(&w.buf).utf(3).is_err());
        assert_eq!(Reader::new(&w.buf).utf(4).unwrap(), "😀😀");
    }

    #[test]
    fn uuid_display() {
        let u = Uuid {
            high: 0x0123_4567_89ab_cdef,
            low: 0xfedc_ba98_7654_3210,
        };
        assert_eq!(u.to_string(), "01234567-89ab-cdef-fedc-ba9876543210");
    }
}
