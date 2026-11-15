//! Bedrock serialized skins: the in-memory form the plugin hands to Pumpkin's `c-player-list`,
//! and a reader for the client's PlayerSkin packet (id 93), which reaches plugins only as raw
//! bytes.

use std::fmt;

/// Bedrock PlayerSkin packet id (both directions).
pub const PLAYER_SKIN_PACKET_ID: i32 = 93;
pub const EMOTE_PACKET_ID: i32 = 138;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PersonaPiece {
    pub piece_id: String,
    pub piece_type: String,
    pub pack_id: String,
    pub is_default: bool,
    pub product_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TintColor {
    pub piece_type: String,
    pub colors: Vec<i32>,
}

/// One Bedrock skin, field for field like the WIT `bedrock-packets.skin` record.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BedrockSkin {
    pub skin_id: String,
    pub play_fab_id: String,
    pub resource_patch: Vec<u8>,
    pub image_width: u32,
    pub image_height: u32,
    /// Raw RGBA.
    pub skin_data: Vec<u8>,
    pub cape_width: u32,
    pub cape_height: u32,
    /// Raw RGBA.
    pub cape_data: Vec<u8>,
    pub geometry_data: Vec<u8>,
    pub geometry_data_engine_version: Vec<u8>,
    pub animation_data: Vec<u8>,
    pub cape_id: String,
    pub full_id: String,
    pub arm_size: String,
    pub skin_color: String,
    pub persona_pieces: Vec<PersonaPiece>,
    pub piece_tint_colors: Vec<TintColor>,
    pub is_premium: bool,
    pub is_persona: bool,
    pub persona_cape_on_classic: bool,
    pub is_primary_user: bool,
    pub override_appearance: bool,
    pub is_trusted: bool,
}

impl BedrockSkin {
    /// Whether the image buffers match their declared sizes.
    #[must_use]
    pub fn images_consistent(&self) -> bool {
        let skin_ok = self.image_width > 0
            && self.image_height > 0
            && self.image_width <= 1024
            && self.image_height <= 1024
            && self.skin_data.len() == (self.image_width * self.image_height * 4) as usize;
        let cape_ok = if self.cape_data.is_empty() {
            true
        } else {
            self.cape_width <= 1024
                && self.cape_height <= 1024
                && self.cape_data.len() == (self.cape_width * self.cape_height * 4) as usize
        };
        skin_ok && cape_ok
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkinParseError(pub String);

impl fmt::Display for SkinParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Little-endian reader with Bedrock's unsigned VarInt lengths.
struct LeReader<'a> {
    d: &'a [u8],
    p: usize,
}

type R<T> = Result<T, SkinParseError>;

/// Caps on anything a reader allocates, so a lying length cannot exhaust memory.
const MAX_BLOB: usize = 4 * 1024 * 1024;
const MAX_LIST: usize = 512;

impl<'a> LeReader<'a> {
    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self
            .p
            .checked_add(n)
            .filter(|&e| e <= self.d.len())
            .ok_or_else(|| SkinParseError("truncated".into()))?;
        let s = &self.d[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> R<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SkinParseError("bad bool".into())),
        }
    }
    fn u32(&mut self) -> R<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("4")))
    }
    fn i32(&mut self) -> R<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().expect("4")))
    }
    fn varuint(&mut self) -> R<u32> {
        let mut v = 0u32;
        for i in 0..5 {
            let b = self.u8()?;
            v |= u32::from(b & 0x7F) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(SkinParseError("VarUInt too long".into()))
    }
    fn blob(&mut self) -> R<Vec<u8>> {
        let n = self.varuint()? as usize;
        if n > MAX_BLOB {
            return Err(SkinParseError("blob too large".into()));
        }
        Ok(self.take(n)?.to_vec())
    }
    fn string(&mut self) -> R<String> {
        String::from_utf8(self.blob()?).map_err(|_| SkinParseError("string is not UTF-8".into()))
    }
    fn count(&mut self, n: u32) -> R<usize> {
        let n = n as usize;
        if n > MAX_LIST {
            Err(SkinParseError("list too long".into()))
        } else {
            Ok(n)
        }
    }
}

/// Field layout of a serialized skin. Pumpkin targets one protocol version; older clients and
/// other server software used an earlier layout. Both are tried and the one whose image sizes
/// add up wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// Pumpkin's current writer (`pumpkin-protocol/src/bedrock/client/player_list.rs`).
    Current { play_fab_id: bool },
    /// The long-standing layout (string arm size and colour, LE-int list counts).
    Legacy,
}

fn read_skin(r: &mut LeReader<'_>, layout: Layout) -> R<BedrockSkin> {
    let mut s = BedrockSkin {
        skin_id: r.string()?,
        ..BedrockSkin::default()
    };
    match layout {
        Layout::Current { play_fab_id } => {
            if play_fab_id {
                s.play_fab_id = r.string()?;
            }
            s.resource_patch = r.blob()?;
            s.image_width = r.u32()?;
            s.image_height = r.u32()?;
            s.skin_data = r.blob()?;
            let n = r.varuint()?;
            for _ in 0..r.count(n)? {
                r.u32()?;
                r.u32()?;
                r.blob()?;
                r.varuint()?;
                r.take(4)?;
                r.varuint()?;
            }
            s.cape_width = r.u32()?;
            s.cape_height = r.u32()?;
            s.cape_data = r.blob()?;
            s.geometry_data = r.blob()?;
            s.geometry_data_engine_version = r.blob()?;
            s.animation_data = r.blob()?;
            s.cape_id = r.string()?;
            s.full_id = r.string()?;
            s.arm_size = if r.u8()? == 0 { "slim" } else { "wide" }.to_owned();
            s.skin_color = format!("#{:x}", r.i32()? as u32);
            let n = r.varuint()?;
            for _ in 0..r.count(n)? {
                let piece_id = r.string()?;
                let piece_type = r.i32()?;
                let pack = r.take(16)?;
                s.persona_pieces.push(PersonaPiece {
                    piece_id,
                    piece_type: piece_type.to_string(),
                    pack_id: format_uuid_bytes(pack),
                    is_default: r.bool()?,
                    product_id: r.string()?,
                });
            }
            let n = r.varuint()?;
            for _ in 0..r.count(n)? {
                let piece_type = r.string()?;
                let mut colors = Vec::with_capacity(4);
                for _ in 0..4 {
                    colors.push(r.i32()?);
                }
                s.piece_tint_colors.push(TintColor { piece_type, colors });
            }
            s.is_premium = r.bool()?;
            s.is_persona = r.bool()?;
            s.persona_cape_on_classic = r.bool()?;
            s.is_primary_user = r.bool()?;
            s.override_appearance = r.bool()?;
            // Pumpkin writes this as a string; accept either.
            let save = r.p;
            s.is_trusted = match r.string() {
                Ok(t) if t == "true" || t == "false" => t == "true",
                _ => {
                    r.p = save;
                    r.bool().unwrap_or(true)
                }
            };
            let _profile_hash = r.string().unwrap_or_default();
        }
        Layout::Legacy => {
            s.play_fab_id = r.string()?;
            s.resource_patch = r.blob()?;
            s.image_width = r.u32()?;
            s.image_height = r.u32()?;
            s.skin_data = r.blob()?;
            let n = r.u32()?;
            for _ in 0..r.count(n)? {
                r.u32()?;
                r.u32()?;
                r.blob()?;
                r.u32()?;
                r.take(4)?;
                r.u32()?;
            }
            s.cape_width = r.u32()?;
            s.cape_height = r.u32()?;
            s.cape_data = r.blob()?;
            s.geometry_data = r.blob()?;
            s.geometry_data_engine_version = r.blob()?;
            s.animation_data = r.blob()?;
            s.cape_id = r.string()?;
            s.full_id = r.string()?;
            s.arm_size = r.string()?;
            s.skin_color = r.string()?;
            let n = r.u32()?;
            for _ in 0..r.count(n)? {
                s.persona_pieces.push(PersonaPiece {
                    piece_id: r.string()?,
                    piece_type: r.string()?,
                    pack_id: r.string()?,
                    is_default: r.bool()?,
                    product_id: r.string()?,
                });
            }
            let n = r.u32()?;
            for _ in 0..r.count(n)? {
                let piece_type = r.string()?;
                let c = r.u32()?;
                let mut colors = Vec::new();
                for _ in 0..r.count(c)? {
                    let hex = r.string()?;
                    colors.push(u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0) as i32);
                }
                s.piece_tint_colors.push(TintColor { piece_type, colors });
            }
            s.is_premium = r.bool()?;
            s.is_persona = r.bool()?;
            s.persona_cape_on_classic = r.bool()?;
            s.is_primary_user = r.bool()?;
            s.override_appearance = r.bool().unwrap_or(true);
            s.is_trusted = true;
        }
    }
    Ok(s)
}

fn format_uuid_bytes(b: &[u8]) -> String {
    let h = crate::codec::sha256::hex(b);
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

/// Parses the payload of a serverbound PlayerSkin packet: `uuid, skin, new name, old name, ...`.
/// The UUID is skipped; the caller knows who sent it.
pub fn parse_player_skin_packet(payload: &[u8]) -> Result<BedrockSkin, SkinParseError> {
    // Mojang's protocol docs (SerializedSkin) have no PlayFab id inside the skin, so that layout
    // goes first; the others only exist for older or differing writers.
    let layouts = [
        Layout::Current { play_fab_id: false },
        Layout::Current { play_fab_id: true },
        Layout::Legacy,
    ];
    let mut last = SkinParseError("empty packet".into());
    for layout in layouts {
        let mut r = LeReader {
            d: payload,
            p: 16.min(payload.len()),
        };
        if payload.len() < 16 {
            break;
        }
        match read_skin(&mut r, layout) {
            Ok(skin) if skin.images_consistent() => return Ok(skin),
            Ok(_) => last = SkinParseError(format!("{layout:?}: image sizes do not match")),
            Err(e) => last = SkinParseError(format!("{layout:?}: {e}")),
        }
    }
    Err(last)
}

// ---------------------------------------------------------------------------------------------
// Test helpers: writers for both layouts, so the reader is checked against known bytes.

#[cfg(test)]
pub mod testing {
    use super::BedrockSkin;

    fn vu(out: &mut Vec<u8>, mut v: u32) {
        loop {
            let b = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    fn blob(out: &mut Vec<u8>, b: &[u8]) {
        vu(out, b.len() as u32);
        out.extend_from_slice(b);
    }

    /// Serializes like Pumpkin's `Skin::write`, with the PlayFab id after the skin id.
    #[must_use]
    pub fn write_current(s: &BedrockSkin) -> Vec<u8> {
        let mut o = vec![0u8; 16]; // uuid
        blob(&mut o, s.skin_id.as_bytes());
        blob(&mut o, s.play_fab_id.as_bytes());
        blob(&mut o, &s.resource_patch);
        o.extend_from_slice(&s.image_width.to_le_bytes());
        o.extend_from_slice(&s.image_height.to_le_bytes());
        blob(&mut o, &s.skin_data);
        vu(&mut o, 0);
        o.extend_from_slice(&s.cape_width.to_le_bytes());
        o.extend_from_slice(&s.cape_height.to_le_bytes());
        blob(&mut o, &s.cape_data);
        blob(&mut o, &s.geometry_data);
        blob(&mut o, &s.geometry_data_engine_version);
        blob(&mut o, &s.animation_data);
        blob(&mut o, s.cape_id.as_bytes());
        blob(&mut o, s.full_id.as_bytes());
        o.push(u8::from(s.arm_size != "slim"));
        o.extend_from_slice(&0i32.to_le_bytes());
        vu(&mut o, 0);
        vu(&mut o, 0);
        o.extend_from_slice(&[u8::from(s.is_premium), u8::from(s.is_persona), 0, 0, 1]);
        blob(&mut o, b"true");
        blob(&mut o, b"");
        blob(&mut o, b"new");
        blob(&mut o, b"old");
        o.push(1);
        o
    }

    #[must_use]
    pub fn write_legacy(s: &BedrockSkin) -> Vec<u8> {
        let mut o = vec![0u8; 16];
        blob(&mut o, s.skin_id.as_bytes());
        blob(&mut o, s.play_fab_id.as_bytes());
        blob(&mut o, &s.resource_patch);
        o.extend_from_slice(&s.image_width.to_le_bytes());
        o.extend_from_slice(&s.image_height.to_le_bytes());
        blob(&mut o, &s.skin_data);
        o.extend_from_slice(&0u32.to_le_bytes());
        o.extend_from_slice(&s.cape_width.to_le_bytes());
        o.extend_from_slice(&s.cape_height.to_le_bytes());
        blob(&mut o, &s.cape_data);
        blob(&mut o, &s.geometry_data);
        blob(&mut o, &s.geometry_data_engine_version);
        blob(&mut o, &s.animation_data);
        blob(&mut o, s.cape_id.as_bytes());
        blob(&mut o, s.full_id.as_bytes());
        blob(&mut o, s.arm_size.as_bytes());
        blob(&mut o, b"#0");
        o.extend_from_slice(&0u32.to_le_bytes());
        o.extend_from_slice(&0u32.to_le_bytes());
        o.extend_from_slice(&[u8::from(s.is_premium), u8::from(s.is_persona), 0, 0, 1]);
        blob(&mut o, b"new");
        blob(&mut o, b"old");
        o
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn sample() -> BedrockSkin {
        BedrockSkin {
            skin_id: "c18e65aa-7b21-4637-9b63-8ad63622ef01.Custom".into(),
            resource_patch: br#"{"geometry":{"default":"geometry.humanoid.custom"}}"#.to_vec(),
            image_width: 64,
            image_height: 64,
            skin_data: (0..64 * 64 * 4).map(|i| (i % 251) as u8).collect(),
            cape_width: 64,
            cape_height: 32,
            cape_data: vec![7; 64 * 32 * 4],
            geometry_data: b"{}".to_vec(),
            full_id: "full".into(),
            arm_size: "wide".into(),
            ..BedrockSkin::default()
        }
    }

    #[test]
    fn parses_current_layout() {
        let s = sample();
        let got = parse_player_skin_packet(&write_current(&s)).unwrap();
        assert_eq!(got.skin_data, s.skin_data);
        assert_eq!(got.cape_data, s.cape_data);
        assert_eq!(got.resource_patch, s.resource_patch);
        assert_eq!(got.full_id, "full");
        assert_eq!(got.arm_size, "wide");
    }

    #[test]
    fn parses_legacy_layout() {
        let mut s = sample();
        s.arm_size = "slim".into();
        let got = parse_player_skin_packet(&write_legacy(&s)).unwrap();
        assert_eq!(got.skin_data, s.skin_data);
        assert_eq!(got.geometry_data, b"{}");
        assert_eq!(got.arm_size, "slim");
    }

    #[test]
    fn garbage_is_rejected_without_panicking() {
        let good = write_current(&sample());
        for n in 0..good.len().min(600) {
            assert!(parse_player_skin_packet(&good[..n]).is_err());
        }
        let mut x = 99u32;
        for _ in 0..200 {
            let v: Vec<u8> = (0..300)
                .map(|_| {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (x >> 16) as u8
                })
                .collect();
            let _ = parse_player_skin_packet(&v);
        }
    }
}

/// What the plugin needs from a Bedrock `Emote` packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmotePacket {
    /// The emote piece UUID.
    pub emote_id: String,
    pub length_ticks: u32,
    pub flags: u8,
}

/// Reads a serverbound `Emote` packet (138) from its raw payload: runtime entity id (VarULong),
/// emote id, length in ticks (VarUInt), XUID, platform id, flags. Pumpkin's plugin host does not
/// convert this packet, so the plugin reads it itself.
pub fn parse_emote_packet(payload: &[u8]) -> Result<EmotePacket, SkinParseError> {
    let mut r = LeReader { d: payload, p: 0 };
    for i in 0..10 {
        if r.u8()? & 0x80 == 0 {
            break;
        }
        if i == 9 {
            return Err(SkinParseError("VarULong too long".into()));
        }
    }
    let emote_id = r.string()?;
    let length_ticks = r.varuint()?;
    r.string()?;
    r.string()?;
    let flags = r.u8().unwrap_or(0);
    Ok(EmotePacket {
        emote_id,
        length_ticks,
        flags,
    })
}
