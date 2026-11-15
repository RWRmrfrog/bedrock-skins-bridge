//! DEFLATE (RFC 1951) with the gzip (RFC 1952) and zlib (RFC 1950) wrappers.
//!
//! The mod GZIP-compresses geometry and emote JSON; PNG wraps its pixels in zlib. Decoding is a
//! complete inflater with an output cap (so a small bomb cannot balloon). Encoding is LZ77 with
//! fixed Huffman codes: simple, and it keeps typical geometry JSON well under the mod's 256 KiB
//! compressed limit.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InflateError {
    Truncated,
    Invalid(&'static str),
    TooLarge,
    Checksum,
}

impl fmt::Display for InflateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("compressed data is truncated"),
            Self::Invalid(why) => write!(f, "invalid compressed data: {why}"),
            Self::TooLarge => f.write_str("decompressed data exceeds the limit"),
            Self::Checksum => f.write_str("checksum mismatch"),
        }
    }
}

impl std::error::Error for InflateError {}

// ---------------------------------------------------------------------------------------------
// Checksums

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

/// CRC-32 (ISO-HDLC), as used by gzip and PNG. `crc` is the running value (0 to start).
#[must_use]
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in data {
        c = CRC_TABLE[((c ^ u32::from(b)) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

#[must_use]
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

// ---------------------------------------------------------------------------------------------
// Inflate

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13,
];
const CLEN_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u64,
    nbits: u32,
}

impl<'a> BitReader<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            bit: 0,
            nbits: 0,
        }
    }

    fn need(&mut self, n: u32) -> Result<(), InflateError> {
        while self.nbits < n {
            let Some(&b) = self.data.get(self.pos) else {
                return Err(InflateError::Truncated);
            };
            self.pos += 1;
            self.bit |= u64::from(b) << self.nbits;
            self.nbits += 8;
        }
        Ok(())
    }

    fn bits(&mut self, n: u32) -> Result<u32, InflateError> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = (self.bit & ((1u64 << n) - 1)) as u32;
        self.bit >>= n;
        self.nbits -= n;
        Ok(v)
    }

    fn align_byte(&mut self) {
        let drop = self.nbits % 8;
        self.bit >>= drop;
        self.nbits -= drop;
    }

    /// Bytes fully consumed from the input (after aligning), for trailers that follow.
    const fn byte_pos(&self) -> usize {
        self.pos - (self.nbits / 8) as usize
    }
}

/// Canonical Huffman decoding table (counts per length + symbols sorted by code).
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self, InflateError> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut left: i32 = 1;
        for &c in &counts[1..] {
            left <<= 1;
            left -= i32::from(c);
            if left < 0 {
                return Err(InflateError::Invalid("over-subscribed code"));
            }
        }
        let mut offs = [0u16; 16];
        for i in 1..15 {
            offs[i + 1] = offs[i] + counts[i];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Self { counts, symbols })
    }

    fn decode(&self, br: &mut BitReader<'_>) -> Result<u16, InflateError> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= br.bits(1)? as i32;
            let count = i32::from(self.counts[len]);
            if code - count < first {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(InflateError::Invalid("bad Huffman code"))
    }
}

fn fixed_tables() -> (Huffman, Huffman) {
    let mut lit = [0u8; 288];
    lit[..144].fill(8);
    lit[144..256].fill(9);
    lit[256..280].fill(7);
    lit[280..].fill(8);
    (
        Huffman::new(&lit).expect("fixed lit"),
        Huffman::new(&[5u8; 30]).expect("fixed dist"),
    )
}

fn dynamic_tables(br: &mut BitReader<'_>) -> Result<(Huffman, Huffman), InflateError> {
    let hlit = br.bits(5)? as usize + 257;
    let hdist = br.bits(5)? as usize + 1;
    let hclen = br.bits(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err(InflateError::Invalid("bad table sizes"));
    }
    let mut clen = [0u8; 19];
    for &i in &CLEN_ORDER[..hclen] {
        clen[i] = br.bits(3)? as u8;
    }
    let clh = Huffman::new(&clen)?;
    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0;
    while i < hlit + hdist {
        let sym = clh.decode(br)?;
        match sym {
            0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            16 => {
                if i == 0 {
                    return Err(InflateError::Invalid("repeat with no previous length"));
                }
                let prev = lengths[i - 1];
                let n = 3 + br.bits(2)? as usize;
                if i + n > lengths.len() {
                    return Err(InflateError::Invalid("repeat overflows"));
                }
                lengths[i..i + n].fill(prev);
                i += n;
            }
            17 | 18 => {
                let n = if sym == 17 { 3 + br.bits(3)? } else { 11 + br.bits(7)? } as usize;
                if i + n > lengths.len() {
                    return Err(InflateError::Invalid("repeat overflows"));
                }
                i += n; // already zero
            }
            _ => return Err(InflateError::Invalid("bad code-length symbol")),
        }
    }
    if lengths[256] == 0 {
        return Err(InflateError::Invalid("missing end-of-block code"));
    }
    Ok((Huffman::new(&lengths[..hlit])?, Huffman::new(&lengths[hlit..])?))
}

/// Raw DEFLATE decode. Returns the output and the number of input bytes consumed.
pub fn inflate_raw(data: &[u8], max_out: usize) -> Result<(Vec<u8>, usize), InflateError> {
    let mut br = BitReader::new(data);
    let mut out: Vec<u8> = Vec::with_capacity(data.len().saturating_mul(3).min(max_out));
    loop {
        let last = br.bits(1)? == 1;
        match br.bits(2)? {
            0 => {
                br.align_byte();
                let len = br.bits(16)? as u16;
                let nlen = br.bits(16)? as u16;
                if len != !nlen {
                    return Err(InflateError::Invalid("stored length mismatch"));
                }
                // The reader holds no buffered bits after reading two aligned u16s.
                debug_assert_eq!(br.nbits, 0);
                let start = br.pos;
                let end = start + len as usize;
                let Some(chunk) = data.get(start..end) else {
                    return Err(InflateError::Truncated);
                };
                if out.len() + chunk.len() > max_out {
                    return Err(InflateError::TooLarge);
                }
                out.extend_from_slice(chunk);
                br.pos = end;
            }
            t @ (1 | 2) => {
                let (lit, dist) = if t == 1 {
                    fixed_tables()
                } else {
                    dynamic_tables(&mut br)?
                };
                loop {
                    let sym = lit.decode(&mut br)?;
                    if sym < 256 {
                        if out.len() >= max_out {
                            return Err(InflateError::TooLarge);
                        }
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let idx = (sym - 257) as usize;
                        if idx >= 29 {
                            return Err(InflateError::Invalid("bad length symbol"));
                        }
                        let len = LEN_BASE[idx] as usize + br.bits(u32::from(LEN_EXTRA[idx]))? as usize;
                        let dsym = dist.decode(&mut br)? as usize;
                        if dsym >= 30 {
                            return Err(InflateError::Invalid("bad distance symbol"));
                        }
                        let d = DIST_BASE[dsym] as usize + br.bits(u32::from(DIST_EXTRA[dsym]))? as usize;
                        if d > out.len() {
                            return Err(InflateError::Invalid("distance too far back"));
                        }
                        if out.len() + len > max_out {
                            return Err(InflateError::TooLarge);
                        }
                        let from = out.len() - d;
                        for k in 0..len {
                            let b = out[from + k];
                            out.push(b);
                        }
                    }
                }
            }
            _ => return Err(InflateError::Invalid("reserved block type")),
        }
        if last {
            break;
        }
    }
    br.align_byte();
    Ok((out, br.byte_pos()))
}

// ---------------------------------------------------------------------------------------------
// Deflate (LZ77 + fixed Huffman)

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, bits: u32) {
        self.acc |= u64::from(value) << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Huffman codes go out most-significant bit first.
    fn put_code(&mut self, code: u32, bits: u32) {
        let mut rev = 0;
        for i in 0..bits {
            rev |= ((code >> i) & 1) << (bits - 1 - i);
        }
        self.put(rev, bits);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

fn put_literal(w: &mut BitWriter, sym: u32) {
    match sym {
        0..=143 => w.put_code(0x30 + sym, 8),
        144..=255 => w.put_code(0x190 + sym - 144, 9),
        256..=279 => w.put_code(sym - 256, 7),
        _ => w.put_code(0xC0 + sym - 280, 8),
    }
}

fn put_match(w: &mut BitWriter, len: usize, dist: usize) {
    let li = LEN_BASE.iter().rposition(|&b| b as usize <= len).expect("len >= 3");
    put_literal(w, 257 + li as u32);
    w.put((len - LEN_BASE[li] as usize) as u32, u32::from(LEN_EXTRA[li]));
    let di = DIST_BASE.iter().rposition(|&b| b as usize <= dist).expect("dist >= 1");
    w.put_code(di as u32, 5);
    w.put((dist - DIST_BASE[di] as usize) as u32, u32::from(DIST_EXTRA[di]));
}

/// Raw DEFLATE encode: one final fixed-Huffman block.
#[must_use]
pub fn deflate_raw(data: &[u8]) -> Vec<u8> {
    const WINDOW: usize = 32 * 1024;
    const HASH_BITS: u32 = 15;
    const MAX_CHAIN: usize = 48;
    const NIL: u32 = u32::MAX;

    let mut w = BitWriter {
        out: Vec::with_capacity(data.len() / 3 + 16),
        acc: 0,
        n: 0,
    };
    w.put(1, 1); // BFINAL
    w.put(1, 2); // fixed Huffman

    let hash = |i: usize| -> usize {
        let v = u32::from(data[i]) << 16 | u32::from(data[i + 1]) << 8 | u32::from(data[i + 2]);
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    };
    let mut head = vec![NIL; 1 << HASH_BITS];
    let mut prev = vec![NIL; data.len()];
    let insert = |i: usize, head: &mut Vec<u32>, prev: &mut Vec<u32>| {
        if i + 2 < data.len() {
            let h = hash(i);
            prev[i] = head[h];
            head[h] = i as u32;
        }
    };

    let mut i = 0;
    while i < data.len() {
        let mut best_len = 0;
        let mut best_dist = 0;
        if i + 2 < data.len() {
            let mut cand = head[hash(i)];
            let mut chain = 0;
            let max_len = (data.len() - i).min(258);
            while cand != NIL && chain < MAX_CHAIN {
                let c = cand as usize;
                if i - c > WINDOW {
                    break;
                }
                if data[c + best_len.min(max_len - 1)] == data[i + best_len.min(max_len - 1)] {
                    let mut l = 0;
                    while l < max_len && data[c + l] == data[i + l] {
                        l += 1;
                    }
                    if l > best_len {
                        best_len = l;
                        best_dist = i - c;
                        if l == max_len {
                            break;
                        }
                    }
                }
                cand = prev[c];
                chain += 1;
            }
        }
        if best_len >= 3 {
            put_match(&mut w, best_len, best_dist);
            for k in i..i + best_len {
                insert(k, &mut head, &mut prev);
            }
            i += best_len;
        } else {
            put_literal(&mut w, u32::from(data[i]));
            insert(i, &mut head, &mut prev);
            i += 1;
        }
    }
    put_literal(&mut w, 256);
    w.finish()
}

// ---------------------------------------------------------------------------------------------
// Wrappers

/// gzip-compress, as `java.util.zip.GZIPOutputStream` would (header fields aside).
#[must_use]
pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    out.extend_from_slice(&deflate_raw(data));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// gzip-decompress a single member, refusing output beyond `max_out`.
pub fn gunzip(data: &[u8], max_out: usize) -> Result<Vec<u8>, InflateError> {
    if data.len() < 18 {
        return Err(InflateError::Truncated);
    }
    if data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 {
        return Err(InflateError::Invalid("not gzip"));
    }
    let flags = data[3];
    let mut p = 10;
    let take = |p: &mut usize, n: usize| -> Result<(), InflateError> {
        *p = p
            .checked_add(n)
            .filter(|&e| e <= data.len())
            .ok_or(InflateError::Truncated)?;
        Ok(())
    };
    if flags & 4 != 0 {
        take(&mut p, 2)?;
        let xlen = u16::from_le_bytes([data[p - 2], data[p - 1]]) as usize;
        take(&mut p, xlen)?;
    }
    for bit in [8u8, 16] {
        if flags & bit != 0 {
            let end = data[p..].iter().position(|&b| b == 0).ok_or(InflateError::Truncated)?;
            take(&mut p, end + 1)?;
        }
    }
    if flags & 2 != 0 {
        take(&mut p, 2)?;
    }
    let (out, used) = inflate_raw(&data[p..], max_out)?;
    let t = p + used;
    let Some(trailer) = data.get(t..t + 8) else {
        return Err(InflateError::Truncated);
    };
    let crc = u32::from_le_bytes(trailer[..4].try_into().expect("4"));
    let isize = u32::from_le_bytes(trailer[4..].try_into().expect("4"));
    if crc != crc32(&out) || isize != out.len() as u32 {
        return Err(InflateError::Checksum);
    }
    Ok(out)
}

#[must_use]
pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    out.extend_from_slice(&deflate_raw(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

pub fn unzlib(data: &[u8], max_out: usize) -> Result<Vec<u8>, InflateError> {
    if data.len() < 6 {
        return Err(InflateError::Truncated);
    }
    let (cmf, flg) = (data[0], data[1]);
    if cmf & 0x0F != 8 || (u16::from(cmf) << 8 | u16::from(flg)) % 31 != 0 || flg & 0x20 != 0 {
        return Err(InflateError::Invalid("bad zlib header"));
    }
    let (out, used) = inflate_raw(&data[2..], max_out)?;
    if let Some(sum) = data.get(2 + used..2 + used + 4)
        && u32::from_be_bytes(sum.try_into().expect("4")) != adler32(&out)
    {
        return Err(InflateError::Checksum);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(n: usize) -> Vec<u8> {
        let words = [
            "\"origin\":[",
            "-4,",
            "24,",
            "\"size\":[8,8,8]",
            "\"uv\":[0,0]",
            "}",
            "{",
            "bone",
        ];
        let mut v = Vec::new();
        let mut x = 12345u32;
        while v.len() < n {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            v.extend_from_slice(words[(x >> 16) as usize % words.len()].as_bytes());
            if x % 7 == 0 {
                v.push((x >> 8) as u8);
            }
        }
        v.truncate(n);
        v
    }

    #[test]
    fn crc_and_adler_vectors() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn roundtrip_various_sizes() {
        for n in [0, 1, 2, 3, 4, 100, 258, 259, 5000, 70_000, 300_000] {
            let data = sample(n);
            assert_eq!(gunzip(&gzip(&data), usize::MAX).unwrap(), data, "gzip n={n}");
            assert_eq!(unzlib(&zlib(&data), usize::MAX).unwrap(), data, "zlib n={n}");
        }
    }

    #[test]
    fn long_runs_compress() {
        let data = vec![b'a'; 100_000];
        let z = gzip(&data);
        assert!(z.len() < 2000, "{}", z.len());
        assert_eq!(gunzip(&z, usize::MAX).unwrap(), data);
    }

    #[test]
    fn output_cap_is_enforced() {
        let z = gzip(&vec![0u8; 10_000]);
        assert_eq!(gunzip(&z, 9_999), Err(InflateError::TooLarge));
        assert!(gunzip(&z, 10_000).is_ok());
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        let mut x = 7u32;
        for len in 0..400 {
            let mut buf = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
            for _ in 0..len {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                buf.push((x >> 24) as u8);
            }
            let _ = gunzip(&buf, 1 << 16);
            let _ = unzlib(&buf[8..], 1 << 16);
        }
    }

    #[test]
    fn stored_block_decodes() {
        // Hand-made stored block: BFINAL=1, BTYPE=00, LEN=5.
        let raw = [0x01, 0x05, 0x00, 0xFA, 0xFF, b'h', b'e', b'l', b'l', b'o'];
        assert_eq!(inflate_raw(&raw, 100).unwrap().0, b"hello");
    }
}
