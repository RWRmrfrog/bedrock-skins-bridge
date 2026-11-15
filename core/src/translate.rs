//! Converting skins between the mod's form (geometry JSON + PNG) and Bedrock's (resource patch +
//! geometry + raw RGBA).

use crate::codec::json::Json;
use crate::codec::png::{self, Rgba};
use crate::codec::sha256::{Sha256, hex};
use crate::proto::bedrock_skin::BedrockSkin;
use crate::proto::payloads::{MAX_CAPE_BYTES, MAX_TEXTURE_BYTES};

/// The vanilla wide and slim player geometry, used when a Bedrock skin names one of them
/// without shipping the file. (From Pumpkin's `assets/bedrock/player_geometry.json`.)
const HUMANOID_GEOMETRY: &str = include_str!("humanoid_geometry.json");

/// The mod's skin as stored by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModSkin {
    pub skin_id: String,
    pub geometry: String,
    /// PNG, or the mod's `BSPA` wrapper around a PNG.
    pub texture: Vec<u8>,
    /// PNG or empty.
    pub cape: Vec<u8>,
}

// ---------------------------------------------------------------------------------------------
// Texture helpers

const BSPA_MAGIC: [u8; 4] = [0x42, 0x53, 0x50, 0x41];

/// The PNG inside a texture: the texture itself, or the base image of the mod's
/// `PersonaTexturePayload` (`BSPA`, used for animated or tinted Persona atlases).
#[must_use]
pub fn base_png(texture: &[u8]) -> Option<&[u8]> {
    if png::has_png_signature(texture) {
        return Some(texture);
    }
    if texture.len() >= 9 && texture[..4] == BSPA_MAGIC && (1..=3).contains(&texture[4]) {
        let len = i32::from_be_bytes(texture[5..9].try_into().ok()?);
        let len = usize::try_from(len).ok()?;
        let base = texture.get(9..9 + len)?;
        return png::has_png_signature(base).then_some(base);
    }
    None
}

/// `ServerSkinHandler.isValidCosmeticTexture`: a PNG, or a well-formed `BSPA` payload whose base
/// image and animation strips are PNGs.
#[must_use]
pub fn is_valid_cosmetic_texture(data: &[u8]) -> bool {
    const MAX_IMAGE: usize = 1_048_576;
    if data.is_empty() || data.len() > MAX_IMAGE {
        return false;
    }
    if data.len() < 5 || data[..4] != BSPA_MAGIC {
        return png::has_png_signature(data);
    }
    let mut p = 4usize;
    let int = |p: &mut usize| -> Option<i32> {
        let v = i32::from_be_bytes(data.get(*p..*p + 4)?.try_into().ok()?);
        *p += 4;
        Some(v)
    };
    let bytes = |p: &mut usize| -> Option<&[u8]> {
        let len = usize::try_from(int(p)?).ok().filter(|&l| l <= MAX_IMAGE)?;
        let b = data.get(*p..*p + len)?;
        *p += len;
        Some(b)
    };
    let mut check = || -> Option<bool> {
        let version = *data.get(4)?;
        if !(1..=3).contains(&version) {
            return None;
        }
        p = 5;
        let base = bytes(&mut p)?;
        if version >= 2 {
            bytes(&mut p)?;
            int(&mut p)?;
        }
        let count = int(&mut p)?;
        if !(0..=32).contains(&count) {
            return None;
        }
        let mut ok = png::has_png_signature(base);
        for _ in 0..count {
            let (x, y, w, h, frames) = (int(&mut p)?, int(&mut p)?, int(&mut p)?, int(&mut p)?, int(&mut p)?);
            if x < 0 || y < 0 || w <= 0 || h <= 0 || !(2..=256).contains(&frames) {
                return None;
            }
            ok &= png::has_png_signature(bytes(&mut p)?);
            if version >= 2 {
                bytes(&mut p)?;
            }
            if version >= 3 && !(0..=1).contains(&int(&mut p)?) {
                return None;
            }
        }
        Some(ok)
    };
    check().unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------
// Geometry helpers

/// The identifier of the (first) geometry in a geometry file.
#[must_use]
pub fn geometry_identifier(geometry: &Json) -> Option<String> {
    if let Some(Json::Arr(list)) = geometry.get("minecraft:geometry") {
        return list
            .iter()
            .find_map(|g| g.get("description")?.get("identifier")?.as_str().map(str::to_owned));
    }
    if let Json::Obj(fields) = geometry {
        return fields
            .iter()
            .find(|(k, _)| k.starts_with("geometry."))
            .map(|(k, _)| k.split(':').next().unwrap_or(k).to_owned());
    }
    None
}

/// Finds the geometry called `name` in a Bedrock geometry file, in either the 1.12+ format or
/// the legacy 1.8 format (where `"geometry.name:geometry.parent"` keys mean inheritance).
fn find_geometry_node<'a>(file: &'a Json, name: &str) -> Option<&'a Json> {
    if let Some(Json::Arr(list)) = file.get("minecraft:geometry") {
        if let Some(found) = list.iter().find(|g| {
            g.get("description")
                .and_then(|d| d.get("identifier"))
                .and_then(Json::as_str)
                == Some(name)
        }) {
            return Some(found);
        }
    }
    if let Json::Obj(fields) = file {
        let prefix = format!("{name}:");
        return fields
            .iter()
            .find(|(k, _)| k == name || k.starts_with(&prefix))
            .map(|(_, v)| v);
    }
    None
}

/// The node wrapped as the mod's client does it (`SkinPackLoader.wrapGeometry`).
fn wrap_geometry(mut node: Json, name: &str) -> Json {
    if let Json::Obj(fields) = &mut node
        && !fields.iter().any(|(k, _)| k == "description")
    {
        let dim = |a: &str, b: &str, fields: &[(String, Json)]| -> String {
            fields
                .iter()
                .find(|(k, _)| k == a || k == b)
                .and_then(|(_, v)| v.as_i64())
                .unwrap_or(64)
                .to_string()
        };
        let desc = Json::Obj(vec![
            ("identifier".into(), Json::Str(name.into())),
            (
                "texture_width".into(),
                Json::Num(dim("texturewidth", "texture_width", fields)),
            ),
            (
                "texture_height".into(),
                Json::Num(dim("textureheight", "texture_height", fields)),
            ),
        ]);
        fields.push(("description".into(), desc));
    }
    Json::Obj(vec![
        ("format_version".into(), Json::Str("1.12.0".into())),
        ("minecraft:geometry".into(), Json::Arr(vec![node])),
    ])
}

/// The geometry name a Bedrock resource patch selects (`{"geometry":{"default":"..."}}`).
#[must_use]
pub fn resource_patch_geometry(patch: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(patch).ok()?;
    let v = Json::parse(text).ok()?;
    v.get("geometry")?.get("default")?.as_str().map(str::to_owned)
}

// ---------------------------------------------------------------------------------------------
// Mod -> Bedrock

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateError(pub String);

impl std::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn terr<T>(msg: impl Into<String>) -> Result<T, TranslateError> {
    Err(TranslateError(msg.into()))
}

/// Builds the Bedrock skin for a mod skin. `hash` is the mod's content hash, used to give the
/// skin a stable id so Bedrock's skin cache never mixes two different skins up.
pub fn mod_to_bedrock(skin: &ModSkin, hash: &str) -> Result<BedrockSkin, TranslateError> {
    let Some(png_bytes) = base_png(&skin.texture) else {
        return terr("texture is not a PNG");
    };
    let img = png::decode(png_bytes).map_err(|e| TranslateError(e.to_string()))?;
    let cape = if skin.cape.is_empty() {
        None
    } else {
        Some(png::decode(&skin.cape).map_err(|e| TranslateError(format!("cape: {e}")))?)
    };

    let geometry = Json::parse(&skin.geometry).map_err(|e| TranslateError(format!("geometry: {e}")))?;
    let Some(identifier) = geometry_identifier(&geometry) else {
        return terr("geometry has no identifier");
    };
    let patch = Json::Obj(vec![(
        "geometry".into(),
        Json::Obj(vec![("default".into(), Json::Str(identifier.clone()))]),
    )]);
    let slim = identifier.to_ascii_lowercase().contains("slim");
    let short = &hash[..hash.len().min(32)];
    let id = format!("bedrockskins-{short}");

    Ok(BedrockSkin {
        skin_id: id.clone(),
        play_fab_id: String::new(),
        resource_patch: patch.to_compact().into_bytes(),
        image_width: img.width,
        image_height: img.height,
        skin_data: img.pixels,
        cape_width: cape.as_ref().map_or(0, |c| c.width),
        cape_height: cape.as_ref().map_or(0, |c| c.height),
        cape_data: cape.map(|c| c.pixels).unwrap_or_default(),
        geometry_data: geometry.to_compact().into_bytes(),
        // "0.0.0" makes the client read the geometry exactly as given (no legacy fix-ups).
        geometry_data_engine_version: b"0.0.0".to_vec(),
        animation_data: Vec::new(),
        cape_id: if skin.cape.is_empty() {
            String::new()
        } else {
            format!("{id}-cape")
        },
        full_id: id,
        arm_size: if slim { "slim" } else { "wide" }.into(),
        skin_color: "#0".into(),
        persona_pieces: Vec::new(),
        piece_tint_colors: Vec::new(),
        // Same flags Pumpkin uses for the skins it sends, so clients render them.
        is_premium: true,
        is_persona: false,
        persona_cape_on_classic: false,
        is_primary_user: false,
        override_appearance: true,
        is_trusted: true,
    })
}

// ---------------------------------------------------------------------------------------------
// Bedrock -> Mod

/// Builds the mod's form of a Bedrock skin: the selected geometry wrapped as a single-geometry
/// file, the skin and cape as PNG.
pub fn bedrock_to_mod(skin: &BedrockSkin) -> Result<ModSkin, TranslateError> {
    if !skin.images_consistent() {
        return terr("skin image does not match its size");
    }
    let Some(name) = resource_patch_geometry(&skin.resource_patch) else {
        return terr("resource patch names no geometry");
    };
    let from_skin = std::str::from_utf8(&skin.geometry_data)
        .ok()
        .filter(|t| !t.trim().is_empty())
        .and_then(|t| Json::parse(t).ok());
    let node = from_skin.as_ref().and_then(|f| find_geometry_node(f, &name)).cloned();
    let node = match node {
        Some(n) => n,
        None => {
            let fallback = Json::parse(HUMANOID_GEOMETRY).expect("bundled geometry parses");
            match find_geometry_node(&fallback, &name) {
                Some(n) => n.clone(),
                None => return terr(format!("geometry {name} not found in the skin")),
            }
        }
    };
    let geometry = wrap_geometry(node, &name).to_compact();

    let texture = png::encode(&Rgba {
        width: skin.image_width,
        height: skin.image_height,
        pixels: skin.skin_data.clone(),
    })
    .map_err(|e| TranslateError(e.to_string()))?;
    if texture.len() > MAX_TEXTURE_BYTES {
        return terr("skin PNG is larger than the mod accepts");
    }
    let cape = if skin.cape_data.is_empty() {
        Vec::new()
    } else {
        let c = png::encode(&Rgba {
            width: skin.cape_width,
            height: skin.cape_height,
            pixels: skin.cape_data.clone(),
        })
        .map_err(|e| TranslateError(format!("cape: {e}")))?;
        if c.len() > MAX_CAPE_BYTES { Vec::new() } else { c }
    };

    // A content-derived id: two Bedrock players with the same skin id but different pixels
    // must not overwrite each other in the mod's client-side skin registry.
    let mut h = Sha256::new();
    h.update(&texture);
    h.update(geometry.as_bytes());
    let digest = hex(&h.finish());
    Ok(ModSkin {
        skin_id: format!("bedrock:{}", &digest[..16]),
        geometry,
        texture,
        cape,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(w: u32, h: u32) -> Vec<u8> {
        (0..w * h * 4).map(|i| (i * 31 % 256) as u8).collect()
    }

    fn mod_skin() -> ModSkin {
        let geometry = r#"{"format_version":"1.12.0","minecraft:geometry":[{"description":{"identifier":"geometry.cat.ears","texture_width":64,"texture_height":64},"bones":[{"name":"head","pivot":[0,24,0],"cubes":[{"origin":[-4,24,-4],"size":[8,8,8],"uv":[0,0]}]}]}]}"#;
        ModSkin {
            skin_id: "pack:cat".into(),
            geometry: geometry.into(),
            texture: png::encode(&Rgba {
                width: 64,
                height: 64,
                pixels: rgba(64, 64),
            })
            .unwrap(),
            cape: png::encode(&Rgba {
                width: 64,
                height: 32,
                pixels: rgba(64, 32),
            })
            .unwrap(),
        }
    }

    #[test]
    fn mod_to_bedrock_builds_a_consistent_skin() {
        let b = mod_to_bedrock(&mod_skin(), &"ab".repeat(32)).unwrap();
        assert!(b.images_consistent());
        assert_eq!(b.skin_data, rgba(64, 64));
        assert_eq!((b.cape_width, b.cape_height), (64, 32));
        assert_eq!(
            resource_patch_geometry(&b.resource_patch).as_deref(),
            Some("geometry.cat.ears")
        );
        assert_eq!(b.arm_size, "wide");
        assert!(b.skin_id.starts_with("bedrockskins-abab"));
    }

    #[test]
    fn roundtrip_through_bedrock_keeps_geometry_and_pixels() {
        let original = mod_skin();
        let b = mod_to_bedrock(&original, "00").unwrap();
        let back = bedrock_to_mod(&b).unwrap();
        assert_eq!(back.geometry, Json::parse(&original.geometry).unwrap().to_compact());
        assert_eq!(png::decode(&back.texture).unwrap().pixels, rgba(64, 64));
        assert_eq!(png::decode(&back.cape).unwrap().pixels, rgba(64, 32));
        assert!(back.skin_id.starts_with("bedrock:"));
    }

    #[test]
    fn legacy_bedrock_geometry_is_wrapped_like_the_mod() {
        let skin = BedrockSkin {
            resource_patch: br#"{"geometry":{"default":"geometry.custom.wings"}}"#.to_vec(),
            image_width: 128,
            image_height: 128,
            skin_data: rgba(128, 128),
            geometry_data: br#"{"format_version":"1.8.0","geometry.custom.wings:geometry.humanoid.custom":{"texturewidth":128,"textureheight":128,"bones":[]}}"#.to_vec(),
            ..BedrockSkin::default()
        };
        let m = bedrock_to_mod(&skin).unwrap();
        let g = Json::parse(&m.geometry).unwrap();
        assert_eq!(geometry_identifier(&g).as_deref(), Some("geometry.custom.wings"));
        let d = g.get("minecraft:geometry").unwrap();
        let Json::Arr(list) = d else { panic!() };
        assert_eq!(
            list[0]
                .get("description")
                .unwrap()
                .get("texture_width")
                .unwrap()
                .as_i64(),
            Some(128)
        );
    }

    #[test]
    fn standard_skin_without_geometry_uses_the_bundled_humanoid() {
        let skin = BedrockSkin {
            resource_patch: br#"{"geometry":{"default":"geometry.humanoid.customSlim"}}"#.to_vec(),
            image_width: 64,
            image_height: 64,
            skin_data: rgba(64, 64),
            ..BedrockSkin::default()
        };
        let m = bedrock_to_mod(&skin).unwrap();
        assert!(m.geometry.contains("geometry.humanoid.customSlim"));
        assert!(m.geometry.contains("\"bones\""));
    }

    #[test]
    fn bspa_wrapper_yields_its_base_png() {
        let png_bytes = mod_skin().texture;
        let mut wrapped = BSPA_MAGIC.to_vec();
        wrapped.push(3);
        wrapped.extend_from_slice(&(png_bytes.len() as i32).to_be_bytes());
        wrapped.extend_from_slice(&png_bytes);
        wrapped.extend_from_slice(&[0, 0, 0, 0]); // empty tint mask, etc.
        assert_eq!(base_png(&wrapped), Some(&png_bytes[..]));
        assert_eq!(base_png(b"nope"), None);
    }

    #[test]
    fn rejects_bad_inputs() {
        let mut s = mod_skin();
        s.texture = b"not a png".to_vec();
        assert!(mod_to_bedrock(&s, "00").is_err());
        let mut s = mod_skin();
        s.geometry = "{}".into();
        assert!(mod_to_bedrock(&s, "00").is_err());
        let b = BedrockSkin {
            image_width: 64,
            image_height: 64,
            skin_data: vec![0; 10],
            ..BedrockSkin::default()
        };
        assert!(bedrock_to_mod(&b).is_err());
    }
}
