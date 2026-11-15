//! Cross-check against a Java reimplementation of the mod's codecs (see `tools/XCheck.java`):
//! `java XCheck make DIR && cargo run --example xcheck DIR && java XCheck verify DIR`.

use bedrock_skins_core::bridge::{Bridge, Out, Platform};
use bedrock_skins_core::codec::png;
use bedrock_skins_core::config::Config;
use bedrock_skins_core::proto::buf::Uuid;
use bedrock_skins_core::proto::payloads::*;
use bedrock_skins_core::translate::{self, ModSkin};
use std::path::Path;

fn expected_rgba(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let a = if (x + y) % 5 == 0 { 0 } else { 255 };
            v.extend_from_slice(&[(x * 4) as u8, (y * 4) as u8, ((x ^ y) * 9) as u8, a]);
        }
    }
    v
}

fn same_visible(got: &[u8], want: &[u8]) -> bool {
    got.chunks(4)
        .zip(want.chunks(4))
        .all(|(g, w)| g[3] == w[3] && (w[3] == 0 || g == w))
}

fn main() {
    let dir = std::env::args().nth(1).expect("dir");
    let dir = Path::new(&dir);
    let java = std::fs::read(dir.join("java_setskin.bin")).unwrap();
    let java_hash = std::fs::read_to_string(dir.join("java_hash.txt")).unwrap();

    // 1. Java-encoded set_skin decodes, and our hash equals the mod's.
    let set = SetSkin::decode(&java).expect("decode Java set_skin");
    let hash = compute_hash(&set.geometry, &set.texture, &set.cape);
    assert_eq!(hash, java_hash.trim(), "hash");
    println!("rust: set_skin from Java decoded, hash matches ({}...)", &hash[..16]);

    // 2. Through the bridge: announce + request/update return the same content.
    let (a, b) = (
        Uuid {
            high: 0x1111,
            low: 0x2222,
        },
        Uuid { high: 3, low: 4 },
    );
    let mut br = Bridge::new(Config::default());
    br.join(a, "A", Platform::Java);
    br.join(b, "B", Platform::Java);
    br.join(Uuid { high: 9, low: 9 }, "Rock", Platform::Bedrock);
    br.channel_registered(b, CH_SKIN_ANNOUNCE);
    let out = br.custom_payload(a, CH_SET_SKIN, &java, 0);
    for o in &out {
        if let Out::Log(l, m) = o {
            println!("log {l:?}: {m}");
        }
    }
    let pushed = out
        .iter()
        .find_map(|o| {
            if let Out::BedrockSkin { skin, .. } = o {
                Some(skin.clone())
            } else {
                None
            }
        })
        .expect("Bedrock push");
    assert_eq!(pushed.image_width, 64);
    assert!(
        same_visible(&pushed.skin_data, &expected_rgba(64, 64)),
        "Java PNG -> Bedrock RGBA"
    );
    assert!(same_visible(&pushed.cape_data, &expected_rgba(64, 32)), "cape RGBA");
    println!("rust: Java ImageIO PNG -> Bedrock RGBA matches pixel for pixel (skin 64x64, cape 64x32)");

    // 3. Bedrock -> mod with our own PNG and gzip, for Java to verify.
    let back: ModSkin = translate::bedrock_to_mod(&pushed).expect("bedrock_to_mod");
    let update = SkinUpdate {
        uuid: a,
        skin_id: Some(back.skin_id.clone()),
        geometry: set.geometry.clone(),
        texture: back.texture.clone(),
        cape: back.cape.clone(),
    };
    std::fs::write(dir.join("rust_update.bin"), update.encode().unwrap()).unwrap();
    let h2 = compute_hash(&update.geometry, &update.texture, &update.cape);
    std::fs::write(dir.join("rust_hash.txt"), &h2).unwrap();
    std::fs::write(
        dir.join("rust_announce.bin"),
        SkinAnnounce {
            uuid: a,
            skin_id: update.skin_id.clone(),
            hash: h2,
        }
        .encode(),
    )
    .unwrap();
    assert!(same_visible(
        &png::decode(&back.texture).unwrap().pixels,
        &expected_rgba(64, 64)
    ));

    let c = Cosmetic {
        id: "hat".into(),
        kind: "persona_head".into(),
        name: "Hât".into(),
        zones: vec!["head".into(), "hair".into()],
        geometry: set.geometry.clone(),
        slim_geometry: "{}".into(),
        texture: back.texture.clone(),
        tint_color: 0x123456,
        side: "both".into(),
    };
    std::fs::write(
        dir.join("rust_cosmetics.bin"),
        CosmeticsUpdate {
            uuid: a,
            cosmetics: vec![c.clone(), c],
        }
        .encode()
        .unwrap(),
    )
    .unwrap();
    let e = PlayEmote {
        id: "wave".into(),
        name: "Wave".into(),
        animation_name: "animation.wave".into(),
        animation: "{\"bones\":{}}".repeat(100),
        duration: 2.5,
    };
    std::fs::write(
        dir.join("rust_emote.bin"),
        EmoteUpdate { uuid: a, emote: e }.encode().unwrap(),
    )
    .unwrap();
    println!("rust: wrote skin_update, skin_announce, cosmetics_update, emote_update for Java");
}
