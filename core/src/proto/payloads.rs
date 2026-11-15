//! The Bedrock Skins mod's custom payloads, byte-for-byte as in `BedrockSkinsNetworking.java`.

use super::buf::{DResult, Reader, Uuid, Writer, err};
use crate::codec::deflate::{InflateError, gunzip, gzip};
use crate::codec::sha256::{Sha256, hex};

pub const CH_SET_SKIN: &str = "bedrockskins:set_skin";
pub const CH_SKIN_UPDATE: &str = "bedrockskins:skin_update";
pub const CH_SKIN_ANNOUNCE: &str = "bedrockskins:skin_announce";
pub const CH_REQUEST_SKIN_DATA: &str = "bedrockskins:request_skin_data";
pub const CH_SET_COSMETICS: &str = "bedrockskins:set_cosmetics";
pub const CH_COSMETICS_UPDATE: &str = "bedrockskins:cosmetics_update";
pub const CH_PLAY_EMOTE: &str = "bedrockskins:play_emote";
pub const CH_EMOTE_UPDATE: &str = "bedrockskins:emote_update";

/// Channels the server receives on; announced to clients with `minecraft:register`, because
/// Fabric's `ClientPlayNetworking.canSend` (and NeoForge's `hasChannel`) refuse to send on a
/// channel the server has not registered.
pub const SERVERBOUND_CHANNELS: [&str; 4] = [CH_SET_SKIN, CH_REQUEST_SKIN_DATA, CH_SET_COSMETICS, CH_PLAY_EMOTE];
/// Channels the server sends on; a client that registers these has the mod.
pub const CLIENTBOUND_CHANNELS: [&str; 4] = [CH_SKIN_UPDATE, CH_SKIN_ANNOUNCE, CH_COSMETICS_UPDATE, CH_EMOTE_UPDATE];

pub const STOP_EMOTE_ID: &str = "bedrockskins:stop";

pub const MAX_GEOMETRY_BYTES: usize = 1_048_576;
pub const MAX_GEOMETRY_COMPRESSED: usize = 262_144;
pub const MAX_TEXTURE_BYTES: usize = 1_048_576;
pub const MAX_CAPE_BYTES: usize = 262_144;
pub const MAX_COSMETICS: usize = 32;
pub const MAX_ZONES: usize = 32;

/// Payload body for `minecraft:register`: channel ids separated by NUL.
#[must_use]
pub fn register_payload(channels: &[&str]) -> Vec<u8> {
    channels.join("\0").into_bytes()
}

// ---------------------------------------------------------------------------------------------
// Field codecs

/// `SkinId.parse(...).toString()`: `"pack"` becomes `"pack:"`, empty means none.
#[must_use]
pub fn normalize_skin_id(raw: &str) -> Option<String> {
    if raw.is_empty() {
        None
    } else if raw.contains(':') {
        Some(raw.to_owned())
    } else {
        Some(format!("{raw}:"))
    }
}

fn read_skin_id(r: &mut Reader<'_>) -> DResult<Option<String>> {
    if r.bool()? {
        Ok(normalize_skin_id(&r.utf(256)?))
    } else {
        Ok(None)
    }
}

fn write_skin_id(w: &mut Writer, id: Option<&str>) {
    match id {
        Some(id) => {
            w.bool(true).utf(id);
        }
        None => {
            w.bool(false);
        }
    }
}

/// `COMPRESSED_GEOMETRY_CODEC.decode`: corrupt data reads as `""` (the mod swallows the
/// `IOException`), but output over 1 MiB fails the packet.
fn read_geometry(r: &mut Reader<'_>) -> DResult<String> {
    let bytes = r.byte_array(MAX_GEOMETRY_COMPRESSED)?;
    if bytes.is_empty() {
        return Ok(String::new());
    }
    match gunzip(bytes, MAX_GEOMETRY_BYTES) {
        Ok(out) => Ok(String::from_utf8_lossy(&out).into_owned()),
        Err(InflateError::TooLarge) => err("geometry JSON exceeded maximum safe length"),
        Err(_) => Ok(String::new()),
    }
}

fn write_geometry(w: &mut Writer, geometry: &str) -> DResult<()> {
    if geometry.is_empty() {
        w.byte_array(&[]);
        return Ok(());
    }
    if geometry.len() > MAX_GEOMETRY_BYTES {
        return err("geometry JSON too large");
    }
    let z = gzip(geometry.as_bytes());
    if z.len() > MAX_GEOMETRY_COMPRESSED {
        return err("compressed geometry too large");
    }
    w.byte_array(&z);
    Ok(())
}

/// `BedrockSkinsNetworking.computeHash(geometry, texture, cape)`.
#[must_use]
pub fn compute_hash(geometry: &str, texture: &[u8], cape: &[u8]) -> String {
    let mut h = Sha256::new();
    let g = geometry.as_bytes();
    h.update(&(g.len() as i32).to_be_bytes());
    h.update(&(texture.len() as i32).to_be_bytes());
    h.update(&(cape.len() as i32).to_be_bytes());
    h.update(g);
    h.update(texture);
    h.update(cape);
    hex(&h.finish())
}

// ---------------------------------------------------------------------------------------------
// Skins

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetSkin {
    pub skin_id: Option<String>,
    pub geometry: String,
    pub texture: Vec<u8>,
    pub cape: Vec<u8>,
}

impl SetSkin {
    pub fn decode(data: &[u8]) -> DResult<Self> {
        let mut r = Reader::new(data);
        let v = Self {
            skin_id: read_skin_id(&mut r)?,
            geometry: read_geometry(&mut r)?,
            texture: r.byte_array(MAX_TEXTURE_BYTES)?.to_vec(),
            cape: r.byte_array(MAX_CAPE_BYTES)?.to_vec(),
        };
        r.finish()?;
        Ok(v)
    }

    pub fn encode(&self) -> DResult<Vec<u8>> {
        let mut w = Writer::new();
        write_skin_id(&mut w, self.skin_id.as_deref());
        write_geometry(&mut w, &self.geometry)?;
        w.byte_array(&self.texture).byte_array(&self.cape);
        Ok(w.into_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkinUpdate {
    pub uuid: Uuid,
    pub skin_id: Option<String>,
    pub geometry: String,
    pub texture: Vec<u8>,
    pub cape: Vec<u8>,
}

impl SkinUpdate {
    pub fn encode(&self) -> DResult<Vec<u8>> {
        if self.texture.len() > MAX_TEXTURE_BYTES || self.cape.len() > MAX_CAPE_BYTES {
            return err("texture too large for skin_update");
        }
        let mut w = Writer::new();
        w.uuid(self.uuid);
        write_skin_id(&mut w, self.skin_id.as_deref());
        write_geometry(&mut w, &self.geometry)?;
        w.byte_array(&self.texture).byte_array(&self.cape);
        Ok(w.into_bytes())
    }

    pub fn decode(data: &[u8]) -> DResult<Self> {
        let mut r = Reader::new(data);
        let v = Self {
            uuid: r.uuid()?,
            skin_id: read_skin_id(&mut r)?,
            geometry: read_geometry(&mut r)?,
            texture: r.byte_array(MAX_TEXTURE_BYTES)?.to_vec(),
            cape: r.byte_array(MAX_CAPE_BYTES)?.to_vec(),
        };
        r.finish()?;
        Ok(v)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkinAnnounce {
    pub uuid: Uuid,
    pub skin_id: Option<String>,
    pub hash: String,
}

impl SkinAnnounce {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.uuid(self.uuid);
        write_skin_id(&mut w, self.skin_id.as_deref());
        w.utf(&self.hash);
        w.into_bytes()
    }

    pub fn decode(data: &[u8]) -> DResult<Self> {
        let mut r = Reader::new(data);
        let v = Self {
            uuid: r.uuid()?,
            skin_id: read_skin_id(&mut r)?,
            hash: r.utf(256)?,
        };
        r.finish()?;
        Ok(v)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSkinData {
    pub hash: String,
}

impl RequestSkinData {
    pub fn decode(data: &[u8]) -> DResult<Self> {
        let mut r = Reader::new(data);
        let v = Self { hash: r.utf(256)? };
        r.finish()?;
        Ok(v)
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.utf(&self.hash);
        w.into_bytes()
    }
}

// ---------------------------------------------------------------------------------------------
// Cosmetics (Persona pieces)

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cosmetic {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub zones: Vec<String>,
    pub geometry: String,
    pub slim_geometry: String,
    pub texture: Vec<u8>,
    pub tint_color: i32,
    pub side: String,
}

fn read_cosmetics(r: &mut Reader<'_>) -> DResult<Vec<Cosmetic>> {
    let n = r.var_int()?;
    if !(0..=MAX_COSMETICS as i32).contains(&n) {
        return err(format!("invalid cosmetic count: {n}"));
    }
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let id = r.utf(128)?;
        let kind = r.utf(64)?;
        let name = r.utf(128)?;
        let zc = r.var_int()?;
        if !(0..=MAX_ZONES as i32).contains(&zc) {
            return err(format!("invalid Persona zone count: {zc}"));
        }
        let mut zones: Vec<String> = Vec::with_capacity(zc as usize);
        for _ in 0..zc {
            let z = r.utf(64)?;
            if !zones.contains(&z) {
                zones.push(z);
            }
        }
        let geometry = read_geometry(r)?;
        let slim = read_geometry(r)?;
        let texture = r.byte_array(MAX_TEXTURE_BYTES)?.to_vec();
        let tint = r.i32()?;
        let side = r.utf(8)?;
        let slim_geometry = if slim.is_empty() { geometry.clone() } else { slim };
        out.push(Cosmetic {
            id,
            kind,
            name,
            zones,
            geometry,
            slim_geometry,
            texture,
            tint_color: tint & 0xFF_FFFF,
            side,
        });
    }
    Ok(out)
}

fn write_cosmetics(w: &mut Writer, list: &[Cosmetic]) -> DResult<()> {
    let list = &list[..list.len().min(MAX_COSMETICS)];
    w.var_int(list.len() as i32);
    for c in list {
        w.utf(&c.id).utf(&c.kind).utf(&c.name);
        let zones = &c.zones[..c.zones.len().min(MAX_ZONES)];
        w.var_int(zones.len() as i32);
        for z in zones {
            w.utf(z);
        }
        write_geometry(w, &c.geometry)?;
        write_geometry(w, &c.slim_geometry)?;
        w.byte_array(&c.texture).i32(c.tint_color).utf(&c.side);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetCosmetics {
    pub cosmetics: Vec<Cosmetic>,
}

impl SetCosmetics {
    pub fn decode(data: &[u8]) -> DResult<Self> {
        let mut r = Reader::new(data);
        let v = Self {
            cosmetics: read_cosmetics(&mut r)?,
        };
        r.finish()?;
        Ok(v)
    }

    pub fn encode(&self) -> DResult<Vec<u8>> {
        let mut w = Writer::new();
        write_cosmetics(&mut w, &self.cosmetics)?;
        Ok(w.into_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CosmeticsUpdate {
    pub uuid: Uuid,
    pub cosmetics: Vec<Cosmetic>,
}

impl CosmeticsUpdate {
    pub fn encode(&self) -> DResult<Vec<u8>> {
        let mut w = Writer::new();
        w.uuid(self.uuid);
        write_cosmetics(&mut w, &self.cosmetics)?;
        Ok(w.into_bytes())
    }

    pub fn decode(data: &[u8]) -> DResult<Self> {
        let mut r = Reader::new(data);
        let v = Self {
            uuid: r.uuid()?,
            cosmetics: read_cosmetics(&mut r)?,
        };
        r.finish()?;
        Ok(v)
    }
}

// ---------------------------------------------------------------------------------------------
// Emotes

#[derive(Debug, Clone, PartialEq)]
pub struct PlayEmote {
    pub id: String,
    pub name: String,
    pub animation_name: String,
    /// Animation JSON; empty for a native Bedrock emote (the mod then looks the id up locally).
    pub animation: String,
    pub duration: f32,
}

impl PlayEmote {
    #[must_use]
    pub fn is_stop(&self) -> bool {
        self.id == STOP_EMOTE_ID
    }

    fn read(r: &mut Reader<'_>) -> DResult<Self> {
        Ok(Self {
            id: r.utf(128)?,
            name: r.utf(128)?,
            animation_name: r.utf(128)?,
            animation: read_geometry(r)?,
            duration: r.f32()?,
        })
    }

    fn write(&self, w: &mut Writer) -> DResult<()> {
        w.utf(&self.id).utf(&self.name).utf(&self.animation_name);
        write_geometry(w, &self.animation)?;
        w.f32(self.duration);
        Ok(())
    }

    pub fn decode(data: &[u8]) -> DResult<Self> {
        let mut r = Reader::new(data);
        let v = Self::read(&mut r)?;
        r.finish()?;
        Ok(v)
    }

    pub fn encode(&self) -> DResult<Vec<u8>> {
        let mut w = Writer::new();
        self.write(&mut w)?;
        Ok(w.into_bytes())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmoteUpdate {
    pub uuid: Uuid,
    pub emote: PlayEmote,
}

impl EmoteUpdate {
    pub fn encode(&self) -> DResult<Vec<u8>> {
        let mut w = Writer::new();
        w.uuid(self.uuid);
        self.emote.write(&mut w)?;
        Ok(w.into_bytes())
    }

    pub fn decode(data: &[u8]) -> DResult<Self> {
        let mut r = Reader::new(data);
        let v = Self {
            uuid: r.uuid()?,
            emote: PlayEmote::read(&mut r)?,
        };
        r.finish()?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const U: Uuid = Uuid {
        high: 0x1122_3344_5566_7788,
        low: 0x99aa_bbcc_ddee_ff00,
    };

    #[test]
    fn hash_has_field_boundaries() {
        // Same concatenation, different split: hashes must differ.
        assert_ne!(compute_hash("ab", b"c", b""), compute_hash("a", b"bc", b""));
        assert_eq!(compute_hash("", b"", b"").len(), 64);
    }

    #[test]
    fn skin_payloads_roundtrip() {
        let set = SetSkin {
            skin_id: Some("pack:skin".into()),
            geometry: "{\"a\":1}".repeat(500),
            texture: vec![0x89, b'P', b'N', b'G', 1],
            cape: vec![],
        };
        assert_eq!(SetSkin::decode(&set.encode().unwrap()).unwrap(), set);
        let up = SkinUpdate {
            uuid: U,
            skin_id: None,
            geometry: String::new(),
            texture: vec![1, 2],
            cape: vec![3],
        };
        assert_eq!(SkinUpdate::decode(&up.encode().unwrap()).unwrap(), up);
        let an = SkinAnnounce {
            uuid: U,
            skin_id: Some("x:y".into()),
            hash: "ab".into(),
        };
        assert_eq!(SkinAnnounce::decode(&an.encode()).unwrap(), an);
        let rq = RequestSkinData { hash: "ff".into() };
        assert_eq!(RequestSkinData::decode(&rq.encode()).unwrap(), rq);
    }

    #[test]
    fn skin_id_normalizes_like_the_mod() {
        let mut w = Writer::new();
        w.bool(true).utf("lonely");
        let mut r = Reader::new(&w.buf);
        assert_eq!(read_skin_id(&mut r).unwrap().as_deref(), Some("lonely:"));
        let mut w = Writer::new();
        w.bool(true).utf("");
        assert_eq!(read_skin_id(&mut Reader::new(&w.buf)).unwrap(), None);
    }

    #[test]
    fn corrupt_geometry_reads_as_empty_but_oversize_fails() {
        let mut w = Writer::new();
        w.bool(false).byte_array(&[1, 2, 3]).byte_array(&[]).byte_array(&[]);
        assert_eq!(SetSkin::decode(&w.buf).unwrap().geometry, "");
        let big = gzip(&vec![b' '; MAX_GEOMETRY_BYTES + 1]);
        let mut w = Writer::new();
        w.bool(false).byte_array(&big).byte_array(&[]).byte_array(&[]);
        assert!(SetSkin::decode(&w.buf).is_err());
    }

    #[test]
    fn cosmetics_and_emotes_roundtrip() {
        let c = Cosmetic {
            id: "hat".into(),
            kind: "persona_head".into(),
            name: "Hat".into(),
            zones: vec!["head".into()],
            geometry: "{}".into(),
            slim_geometry: "{}".into(),
            texture: vec![0x89, b'P', b'N', b'G'],
            tint_color: 0x00ff_00ff,
            side: "both".into(),
        };
        let up = CosmeticsUpdate {
            uuid: U,
            cosmetics: vec![c.clone(), c],
        };
        assert_eq!(CosmeticsUpdate::decode(&up.encode().unwrap()).unwrap(), up);
        let e = EmoteUpdate {
            uuid: U,
            emote: PlayEmote {
                id: "wave".into(),
                name: "Wave".into(),
                animation_name: "animation.wave".into(),
                animation: "{}".into(),
                duration: 2.5,
            },
        };
        assert_eq!(EmoteUpdate::decode(&e.encode().unwrap()).unwrap(), e);
    }

    #[test]
    fn truncated_payloads_fail_cleanly() {
        let set = SetSkin {
            skin_id: Some("p:s".into()),
            geometry: "{}".into(),
            texture: vec![1; 50],
            cape: vec![2; 9],
        }
        .encode()
        .unwrap();
        for n in 0..set.len() {
            assert!(SetSkin::decode(&set[..n]).is_err());
        }
    }
}
