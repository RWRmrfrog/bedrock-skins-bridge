//! PNG to RGBA8 and back. Bedrock skins travel as raw RGBA; the mod sends PNG files.

use super::deflate::{crc32, crc32_update, unzlib, zlib};
use std::fmt;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// Largest image accepted (Bedrock's biggest skin is 512x512; leave room for capes/atlases).
pub const MAX_DIMENSION: u32 = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major, non-premultiplied.
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PngError(pub &'static str);

impl fmt::Display for PngError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PNG: {}", self.0)
    }
}

impl std::error::Error for PngError {}

#[must_use]
pub fn has_png_signature(data: &[u8]) -> bool {
    data.starts_with(&SIGNATURE[..4])
}

/// Decodes any non-interlaced PNG (all colour types, bit depths 1-16) to RGBA8.
pub fn decode(data: &[u8]) -> Result<Rgba, PngError> {
    if !data.starts_with(&SIGNATURE) {
        return Err(PngError("missing signature"));
    }
    let mut p = 8;
    let mut ihdr: Option<(u32, u32, u8, u8)> = None;
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat = Vec::new();
    loop {
        let header = data.get(p..p + 8).ok_or(PngError("truncated chunk header"))?;
        let len = u32::from_be_bytes(header[..4].try_into().expect("4")) as usize;
        let kind: [u8; 4] = header[4..8].try_into().expect("4");
        let body = data.get(p + 8..p + 8 + len).ok_or(PngError("truncated chunk"))?;
        p += 12 + len;
        match &kind {
            b"IHDR" => {
                if body.len() != 13 {
                    return Err(PngError("bad IHDR"));
                }
                let w = u32::from_be_bytes(body[..4].try_into().expect("4"));
                let h = u32::from_be_bytes(body[4..8].try_into().expect("4"));
                if body[12] != 0 {
                    return Err(PngError("interlaced images are not supported"));
                }
                if w == 0 || h == 0 || w > MAX_DIMENSION || h > MAX_DIMENSION {
                    return Err(PngError("unsupported dimensions"));
                }
                ihdr = Some((w, h, body[8], body[9]));
            }
            b"PLTE" => {
                palette = body.chunks_exact(3).map(|c| [c[0], c[1], c[2], 255]).collect();
            }
            b"tRNS" => trns = body.to_vec(),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
    }
    let (width, height, depth, color) = ihdr.ok_or(PngError("missing IHDR"))?;
    let channels: u32 = match color {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return Err(PngError("bad colour type")),
    };
    let depth_ok = match color {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        _ => matches!(depth, 8 | 16),
    };
    if !depth_ok {
        return Err(PngError("bad bit depth"));
    }
    for (i, &a) in trns.iter().enumerate() {
        if color == 3 && i < palette.len() {
            palette[i][3] = a;
        }
    }

    let bits_pp = channels * u32::from(depth);
    let stride = ((width * bits_pp).div_ceil(8)) as usize;
    let bpp = (bits_pp as usize).div_ceil(8).max(1);
    let expected = (stride + 1) * height as usize;
    let raw = unzlib(&idat, expected).map_err(|_| PngError("bad image data"))?;
    if raw.len() != expected {
        return Err(PngError("image data has the wrong size"));
    }

    let mut cur = vec![0u8; stride];
    let mut prev = vec![0u8; stride];
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height as usize {
        let line = &raw[row * (stride + 1)..(row + 1) * (stride + 1)];
        let filter = line[0];
        cur.copy_from_slice(&line[1..]);
        for i in 0..stride {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            let add = match filter {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                4 => paeth(a, b, c),
                _ => return Err(PngError("bad filter type")),
            };
            cur[i] = cur[i].wrapping_add(add);
        }
        expand_row(&cur, width, depth, color, &palette, &trns, &mut pixels)?;
        std::mem::swap(&mut cur, &mut prev);
    }
    Ok(Rgba { width, height, pixels })
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn expand_row(
    row: &[u8],
    width: u32,
    depth: u8,
    color: u8,
    palette: &[[u8; 4]],
    trns: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), PngError> {
    // Sample n of the row at the image's bit depth, as a raw value.
    let sample = |n: usize| -> u16 {
        match depth {
            16 => u16::from_be_bytes([row[n * 2], row[n * 2 + 1]]),
            8 => u16::from(row[n]),
            d => {
                let d = d as usize;
                let bit = n * d;
                let byte = row[bit / 8];
                let shift = 8 - d - bit % 8;
                u16::from((byte >> shift) & ((1u8 << d) - 1))
            }
        }
    };
    // Scale a sample to 8 bits.
    let to8 = |v: u16| -> u8 {
        match depth {
            16 => (v >> 8) as u8,
            8 => v as u8,
            d => (u32::from(v) * 255 / ((1u32 << d) - 1)) as u8,
        }
    };
    let key = |i: usize| -> Option<u16> { trns.get(i * 2..i * 2 + 2).map(|b| u16::from_be_bytes([b[0], b[1]])) };
    for x in 0..width as usize {
        match color {
            0 => {
                let v = sample(x);
                let a = if key(0) == Some(v) { 0 } else { 255 };
                let g = to8(v);
                out.extend_from_slice(&[g, g, g, a]);
            }
            2 => {
                let (r, g, b) = (sample(x * 3), sample(x * 3 + 1), sample(x * 3 + 2));
                let a = if key(0) == Some(r) && key(1) == Some(g) && key(2) == Some(b) {
                    0
                } else {
                    255
                };
                out.extend_from_slice(&[to8(r), to8(g), to8(b), a]);
            }
            3 => {
                let idx = sample(x) as usize;
                out.extend_from_slice(palette.get(idx).ok_or(PngError("palette index out of range"))?);
            }
            4 => {
                let g = to8(sample(x * 2));
                out.extend_from_slice(&[g, g, g, to8(sample(x * 2 + 1))]);
            }
            _ => {
                for c in 0..4 {
                    out.push(to8(sample(x * 4 + c)));
                }
            }
        }
    }
    Ok(())
}

/// Encodes RGBA8 as a PNG (colour type 6, filter chosen per row).
pub fn encode(img: &Rgba) -> Result<Vec<u8>, PngError> {
    let (w, h) = (img.width as usize, img.height as usize);
    if img.width == 0 || img.height == 0 || img.pixels.len() != w * h * 4 {
        return Err(PngError("pixel buffer does not match the dimensions"));
    }
    let stride = w * 4;
    let mut raw = Vec::with_capacity((stride + 1) * h);
    let zero = vec![0u8; stride];
    for y in 0..h {
        let row = &img.pixels[y * stride..(y + 1) * stride];
        let prev = if y == 0 {
            &zero[..]
        } else {
            &img.pixels[(y - 1) * stride..y * stride]
        };
        // Pick None or Up, whichever leaves more zero bytes (cheap heuristic, good on skins).
        let up_zeros = row.iter().zip(prev).filter(|(a, b)| a == b).count();
        let none_zeros = row.iter().filter(|&&b| b == 0).count();
        if up_zeros > none_zeros {
            raw.push(2);
            raw.extend(row.iter().zip(prev).map(|(a, b)| a.wrapping_sub(*b)));
        } else {
            raw.push(0);
            raw.extend_from_slice(row);
        }
    }
    let mut out = SIGNATURE.to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&img.width.to_be_bytes());
    ihdr.extend_from_slice(&img.height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib(&raw));
    chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out.extend_from_slice(&crc32_update(crc32(kind), body).to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(w: u32, h: u32) -> Rgba {
        let mut pixels = Vec::new();
        for y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(&[
                    (x * 4) as u8,
                    (y * 4) as u8,
                    ((x ^ y) * 9) as u8,
                    if (x + y) % 5 == 0 { 0 } else { 255 },
                ]);
            }
        }
        Rgba {
            width: w,
            height: h,
            pixels,
        }
    }

    #[test]
    fn roundtrip() {
        for (w, h) in [(1, 1), (64, 32), (64, 64), (128, 128), (3, 7)] {
            let img = checker(w, h);
            let png = encode(&img).unwrap();
            assert!(has_png_signature(&png));
            assert_eq!(decode(&png).unwrap(), img);
        }
    }

    #[test]
    fn rejects_mismatched_buffer() {
        assert!(
            encode(&Rgba {
                width: 2,
                height: 2,
                pixels: vec![0; 15]
            })
            .is_err()
        );
    }

    #[test]
    fn garbage_does_not_panic() {
        let png = encode(&checker(16, 16)).unwrap();
        for i in 8..png.len() {
            let mut bad = png.clone();
            bad[i] ^= 0x5A;
            let _ = decode(&bad);
            let _ = decode(&png[..i]);
        }
    }
}
