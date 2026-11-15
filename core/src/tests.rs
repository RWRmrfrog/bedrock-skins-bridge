//! End-to-end scenarios through the `Bridge`, decoding what it sends the way the mod would.

use crate::bridge::{Bridge, Out, Platform, Position, REGISTER_CHANNEL, RESEND_DELAY_TICKS};
use crate::codec::png::{self, Rgba};
use crate::config::Config;
use crate::proto::bedrock_skin::{BedrockSkin, testing::write_current};
use crate::proto::buf::Uuid;
use crate::proto::payloads::{self as p, *};

const JAVA_A: Uuid = Uuid { high: 1, low: 1 };
const JAVA_B: Uuid = Uuid { high: 2, low: 2 };
const VANILLA: Uuid = Uuid { high: 3, low: 3 };
const BEDROCK: Uuid = Uuid { high: 4, low: 4 };

const GEOMETRY: &str = r#"{"format_version":"1.12.0","minecraft:geometry":[{"description":{"identifier":"geometry.fox","texture_width":64,"texture_height":64},"bones":[{"name":"head","pivot":[0,24,0]}]}]}"#;

fn texture() -> Vec<u8> {
    png::encode(&Rgba {
        width: 64,
        height: 64,
        pixels: (0..64 * 64 * 4).map(|i| (i % 7) as u8 * 30).collect(),
    })
    .unwrap()
}

fn set_skin_bytes(id: Option<&str>) -> Vec<u8> {
    SetSkin {
        skin_id: id.map(str::to_owned),
        geometry: GEOMETRY.into(),
        texture: texture(),
        cape: vec![],
    }
    .encode()
    .unwrap()
}

fn payloads<'a>(out: &'a [Out], to: Uuid, channel: &str) -> Vec<&'a [u8]> {
    out.iter()
        .filter_map(|o| match o {
            Out::Payload {
                to: t,
                channel: c,
                data,
            } if *t == to && *c == channel => Some(data.as_slice()),
            _ => None,
        })
        .collect()
}

fn bedrock_pushes(out: &[Out]) -> Vec<(Uuid, Uuid)> {
    out.iter()
        .filter_map(|o| match o {
            Out::BedrockSkin { to, owner, .. } => Some((*to, *owner)),
            _ => None,
        })
        .collect()
}

/// Server with two modded Java players, one vanilla Java player and one Bedrock player.
fn server() -> Bridge {
    let mut b = Bridge::new(Config::default());
    for (u, n) in [(JAVA_A, "Alice"), (JAVA_B, "Bob"), (VANILLA, "Vic")] {
        let out = b.join(u, n, Platform::Java);
        let reg = payloads(&out, u, REGISTER_CHANNEL);
        assert_eq!(reg.len(), 1);
        assert!(
            std::str::from_utf8(reg[0])
                .unwrap()
                .split('\0')
                .any(|c| c == p::CH_SET_SKIN)
        );
    }
    b.join(BEDROCK, "Brock", Platform::Bedrock);
    b.channel_registered(JAVA_A, p::CH_SKIN_ANNOUNCE);
    b.channel_registered(JAVA_B, p::CH_SKIN_UPDATE);
    b
}

#[test]
fn set_skin_announce_request_update_flow() {
    let mut b = server();
    let out = b.custom_payload(JAVA_A, p::CH_SET_SKIN, &set_skin_bytes(Some("pack:fox")), 0);

    // Announced to both modded players (the mod also echoes to the sender), not to vanilla.
    let to_b = payloads(&out, JAVA_B, p::CH_SKIN_ANNOUNCE);
    assert_eq!(to_b.len(), 1);
    assert_eq!(payloads(&out, JAVA_A, p::CH_SKIN_ANNOUNCE).len(), 1);
    assert!(payloads(&out, VANILLA, p::CH_SKIN_ANNOUNCE).is_empty());
    let ann = SkinAnnounce::decode(to_b[0]).unwrap();
    assert_eq!(ann.uuid, JAVA_A);
    assert_eq!(ann.skin_id.as_deref(), Some("pack:fox"));
    assert_eq!(ann.hash, compute_hash(GEOMETRY, &texture(), &[]));

    // Shown to the Bedrock player.
    assert_eq!(bedrock_pushes(&out), vec![(BEDROCK, JAVA_A)]);

    // Bob asks for the content and gets it attributed to Alice.
    let out = b.custom_payload(
        JAVA_B,
        p::CH_REQUEST_SKIN_DATA,
        &RequestSkinData { hash: ann.hash.clone() }.encode(),
        10,
    );
    let up = SkinUpdate::decode(payloads(&out, JAVA_B, p::CH_SKIN_UPDATE)[0]).unwrap();
    assert_eq!(up.uuid, JAVA_A);
    assert_eq!(up.geometry, GEOMETRY);
    assert_eq!(up.texture, texture());
    assert_eq!(compute_hash(&up.geometry, &up.texture, &up.cape), ann.hash);
}

#[test]
fn late_modded_joiner_is_caught_up() {
    let mut b = server();
    b.custom_payload(JAVA_A, p::CH_SET_SKIN, &set_skin_bytes(Some("pack:fox")), 0);
    let late = Uuid { high: 9, low: 9 };
    b.join(late, "Late", Platform::Java);
    let out = b.channel_registered(late, p::CH_EMOTE_UPDATE);
    let ann = payloads(&out, late, p::CH_SKIN_ANNOUNCE);
    assert_eq!(ann.len(), 1);
    assert_eq!(SkinAnnounce::decode(ann[0]).unwrap().uuid, JAVA_A);
    // Registering twice does not resend.
    assert!(b.channel_registered(late, p::CH_SKIN_UPDATE).is_empty());
}

#[test]
fn rate_limit_reset_and_validation() {
    let mut b = server();
    b.custom_payload(JAVA_A, p::CH_SET_SKIN, &set_skin_bytes(Some("pack:fox")), 0);
    // Within 5 s: ignored.
    let out = b.custom_payload(JAVA_A, p::CH_SET_SKIN, &set_skin_bytes(Some("pack:other")), 4_999);
    assert!(payloads(&out, JAVA_B, p::CH_SKIN_ANNOUNCE).is_empty());
    // Reset after the cooldown: announce with no skin and an empty hash.
    let out = b.custom_payload(JAVA_A, p::CH_SET_SKIN, &set_skin_bytes(None), 5_000);
    let ann = SkinAnnounce::decode(payloads(&out, JAVA_B, p::CH_SKIN_ANNOUNCE)[0]).unwrap();
    assert_eq!((ann.skin_id, ann.hash.as_str()), (None, ""));
    // The content is gone.
    let hash = compute_hash(GEOMETRY, &texture(), &[]);
    let out = b.custom_payload(
        JAVA_B,
        p::CH_REQUEST_SKIN_DATA,
        &RequestSkinData { hash }.encode(),
        6_000,
    );
    assert!(payloads(&out, JAVA_B, p::CH_SKIN_UPDATE).is_empty());
    // A non-PNG texture is refused.
    let bad = SetSkin {
        skin_id: Some("p:x".into()),
        geometry: GEOMETRY.into(),
        texture: b"GIF89a".to_vec(),
        cape: vec![],
    };
    let out = b.custom_payload(JAVA_B, p::CH_SET_SKIN, &bad.encode().unwrap(), 0);
    assert!(payloads(&out, JAVA_A, p::CH_SKIN_ANNOUNCE).is_empty());
    // Garbage is logged, not a panic.
    let out = b.custom_payload(JAVA_B, p::CH_SET_SKIN, &[1, 2, 3], 0);
    assert!(out.iter().any(|o| matches!(o, Out::Log(..))));
}

#[test]
fn identical_skins_are_stored_once_and_freed_on_leave() {
    let mut b = server();
    b.custom_payload(JAVA_A, p::CH_SET_SKIN, &set_skin_bytes(Some("pack:fox")), 0);
    b.custom_payload(JAVA_B, p::CH_SET_SKIN, &set_skin_bytes(Some("pack:fox")), 0);
    assert!(b.status()[1].contains("2 active, 1 stored"), "{:?}", b.status());
    b.leave(JAVA_A);
    assert!(b.status()[1].contains("1 active, 1 stored"));
    b.leave(JAVA_B);
    assert!(b.status()[1].contains("0 active, 0 stored"));
}

#[test]
fn cosmetics_are_validated_relayed_and_replayed() {
    let mut b = server();
    let good = Cosmetic {
        id: "hat".into(),
        kind: "persona_head".into(),
        name: "Hat".into(),
        zones: vec![],
        geometry: "{}".into(),
        slim_geometry: "{}".into(),
        texture: texture(),
        tint_color: 0,
        side: "both".into(),
    };
    let out = b.custom_payload(
        JAVA_A,
        p::CH_SET_COSMETICS,
        &SetCosmetics {
            cosmetics: vec![good.clone()],
        }
        .encode()
        .unwrap(),
        0,
    );
    let up = CosmeticsUpdate::decode(payloads(&out, JAVA_B, p::CH_COSMETICS_UPDATE)[0]).unwrap();
    assert_eq!((up.uuid, up.cosmetics.len()), (JAVA_A, 1));

    let late = Uuid { high: 7, low: 7 };
    b.join(late, "Late", Platform::Java);
    let out = b.channel_registered(late, p::CH_COSMETICS_UPDATE);
    let replay = CosmeticsUpdate::decode(payloads(&out, late, p::CH_COSMETICS_UPDATE)[0]).unwrap();
    assert_eq!(replay, up);

    let mut bad = good;
    bad.texture = b"nope".to_vec();
    let out = b.custom_payload(
        JAVA_B,
        p::CH_SET_COSMETICS,
        &SetCosmetics { cosmetics: vec![bad] }.encode().unwrap(),
        0,
    );
    assert!(payloads(&out, JAVA_A, p::CH_COSMETICS_UPDATE).is_empty());
}

#[test]
fn emotes_java_and_bedrock() {
    let mut b = server();
    let wave = PlayEmote {
        id: "wave".into(),
        name: "Wave".into(),
        animation_name: "a".into(),
        animation: "{\"x\":1}".into(),
        duration: 2.0,
    };
    let out = b.custom_payload(JAVA_A, p::CH_PLAY_EMOTE, &wave.encode().unwrap(), 0);
    let e = EmoteUpdate::decode(payloads(&out, JAVA_B, p::CH_EMOTE_UPDATE)[0]).unwrap();
    assert_eq!((e.uuid, e.emote), (JAVA_A, wave.clone()));

    let mut too_long = wave;
    too_long.duration = 31.0;
    assert!(
        b.custom_payload(JAVA_A, p::CH_PLAY_EMOTE, &too_long.encode().unwrap(), 0)
            .is_empty()
    );
    let stop = PlayEmote {
        id: STOP_EMOTE_ID.into(),
        name: String::new(),
        animation_name: String::new(),
        animation: String::new(),
        duration: 0.0,
    };
    assert_eq!(
        payloads(
            &b.custom_payload(JAVA_A, p::CH_PLAY_EMOTE, &stop.encode().unwrap(), 0),
            JAVA_B,
            p::CH_EMOTE_UPDATE
        )
        .len(),
        1
    );

    let out = b.bedrock_emote(BEDROCK, "4c8ae710-df2e-47cd-814d-cc7bf21a3d67", 60);
    let e = EmoteUpdate::decode(payloads(&out, JAVA_A, p::CH_EMOTE_UPDATE)[0]).unwrap();
    assert_eq!(e.uuid, BEDROCK);
    assert_eq!(e.emote.animation, "");
    assert!((e.emote.duration - 3.0).abs() < 1e-6);
}

#[test]
fn bedrock_skin_change_reaches_modded_java_and_other_bedrock() {
    let mut b = server();
    let other_bedrock = Uuid { high: 5, low: 5 };
    b.join(other_bedrock, "Rock2", Platform::Bedrock);
    let skin = BedrockSkin {
        skin_id: "custom".into(),
        resource_patch: br#"{"geometry":{"default":"geometry.humanoid.custom"}}"#.to_vec(),
        image_width: 64,
        image_height: 64,
        skin_data: vec![200; 64 * 64 * 4],
        full_id: "custom_full".into(),
        arm_size: "wide".into(),
        ..BedrockSkin::default()
    };
    let out = b.bedrock_packet(BEDROCK, 93, &write_current(&skin), 0);
    let ann = SkinAnnounce::decode(payloads(&out, JAVA_A, p::CH_SKIN_ANNOUNCE)[0]).unwrap();
    assert_eq!(ann.uuid, BEDROCK);
    assert!(ann.skin_id.unwrap().starts_with("bedrock:"));
    assert_eq!(bedrock_pushes(&out), vec![(other_bedrock, BEDROCK)]);

    let out = b.custom_payload(
        JAVA_A,
        p::CH_REQUEST_SKIN_DATA,
        &RequestSkinData { hash: ann.hash.clone() }.encode(),
        0,
    );
    let up = SkinUpdate::decode(payloads(&out, JAVA_A, p::CH_SKIN_UPDATE)[0]).unwrap();
    assert_eq!(compute_hash(&up.geometry, &up.texture, &up.cape), ann.hash);
    assert_eq!(png::decode(&up.texture).unwrap().pixels, vec![200; 64 * 64 * 4]);
    assert!(up.geometry.contains("geometry.humanoid.custom"));

    // Other packet ids are ignored.
    assert!(b.bedrock_packet(BEDROCK, 1, &[], 10_000).is_empty());
}

#[test]
fn bedrock_resend_when_coming_into_range() {
    let mut b = server();
    b.custom_payload(JAVA_A, p::CH_SET_SKIN, &set_skin_bytes(Some("pack:fox")), 0);
    let at = |u, x: f64, world: &str| Position {
        uuid: u,
        world: world.into(),
        x,
        y: 64.0,
        z: 0.0,
    };

    // Far apart: nothing.
    assert!(b.tick(10, &[at(JAVA_A, 0.0, "w"), at(BEDROCK, 500.0, "w")]).is_empty());
    // Close: queued, sent after the delay, once.
    assert!(b.tick(10, &[at(JAVA_A, 0.0, "w"), at(BEDROCK, 10.0, "w")]).is_empty());
    let mut sent = Vec::new();
    for _ in 0..(RESEND_DELAY_TICKS / 10 + 1) {
        sent.extend(bedrock_pushes(
            &b.tick(10, &[at(JAVA_A, 0.0, "w"), at(BEDROCK, 10.0, "w")]),
        ));
    }
    assert_eq!(sent, vec![(BEDROCK, JAVA_A)]);
    // Same spot, other world: out of range; back again: re-sent.
    b.tick(10, &[at(JAVA_A, 0.0, "nether"), at(BEDROCK, 10.0, "w")]);
    let mut sent = Vec::new();
    for _ in 0..4 {
        sent.extend(bedrock_pushes(
            &b.tick(10, &[at(JAVA_A, 0.0, "w"), at(BEDROCK, 10.0, "w")]),
        ));
    }
    assert_eq!(sent, vec![(BEDROCK, JAVA_A)]);
    // A respawn forgets the pair, so it is re-sent while still in range.
    b.forget_tracking(JAVA_A);
    let mut sent = Vec::new();
    for _ in 0..4 {
        sent.extend(bedrock_pushes(
            &b.tick(10, &[at(JAVA_A, 0.0, "w"), at(BEDROCK, 10.0, "w")]),
        ));
    }
    assert_eq!(sent.len(), 1);
}

#[test]
fn bedrock_sync_can_be_disabled() {
    let mut b = Bridge::new(Config {
        bedrock_sync: false,
        ..Config::default()
    });
    b.join(JAVA_A, "A", Platform::Java);
    b.join(BEDROCK, "B", Platform::Bedrock);
    let out = b.custom_payload(JAVA_A, p::CH_SET_SKIN, &set_skin_bytes(Some("pack:fox")), 0);
    assert!(bedrock_pushes(&out).is_empty());
    // The sender counts as modded once it uses a channel.
    assert_eq!(payloads(&out, JAVA_A, p::CH_SKIN_ANNOUNCE).len(), 1);
}

#[test]
fn bedrock_login_skin_reaches_modded_java_and_other_bedrock() {
    let mut b = server();
    let other_bedrock = Uuid { high: 5, low: 5 };
    b.join(other_bedrock, "Rock2", Platform::Bedrock);
    let skin = BedrockSkin {
        skin_id: "login".into(),
        resource_patch: br#"{"geometry":{"default":"geometry.humanoid.custom"}}"#.to_vec(),
        image_width: 64,
        image_height: 64,
        skin_data: vec![90; 64 * 64 * 4],
        arm_size: "wide".into(),
        ..BedrockSkin::default()
    };

    let out = b.bedrock_login_skin(BEDROCK, skin.clone());
    let ann = SkinAnnounce::decode(payloads(&out, JAVA_A, p::CH_SKIN_ANNOUNCE)[0]).unwrap();
    assert_eq!(ann.uuid, BEDROCK);
    assert_eq!(bedrock_pushes(&out), vec![(other_bedrock, BEDROCK)]);

    // A login skin does not start the skin change cooldown.
    let out = b.bedrock_packet(BEDROCK, 93, &write_current(&skin), 0);
    assert!(!payloads(&out, JAVA_A, p::CH_SKIN_ANNOUNCE).is_empty());

    // Image data that does not match its size is dropped with a log line only.
    let mut bad = skin;
    bad.skin_data.truncate(10);
    let out = b.bedrock_login_skin(BEDROCK, bad);
    assert!(!out.is_empty() && out.iter().all(|o| matches!(o, Out::Log(..))));
}

#[test]
fn java_emote_reaches_bedrock_players_and_is_remembered_for_bedrock_emotes() {
    let mut b = server();
    let id = "8181e0f2-46c5-4d47-9c3f-e1480665adfa";
    let dance = PlayEmote {
        id: id.into(),
        name: "Dance".into(),
        animation_name: "animation.dance".into(),
        animation: r#"{"animation_length":2.0,"bones":{}}"#.into(),
        duration: 2.0,
    };
    let bedrock_emotes = |out: &[Out]| -> Vec<(Uuid, Uuid, String, u32)> {
        out.iter()
            .filter_map(|o| match o {
                Out::BedrockEmote { to, owner, emote_id, length_ticks } => {
                    Some((*to, *owner, emote_id.clone(), *length_ticks))
                }
                _ => None,
            })
            .collect()
    };

    let out = b.custom_payload(JAVA_A, p::CH_PLAY_EMOTE, &dance.encode().unwrap(), 0);
    assert_eq!(bedrock_emotes(&out), vec![(BEDROCK, JAVA_A, id.to_owned(), 40)]);
    assert_eq!(payloads(&out, JAVA_B, p::CH_EMOTE_UPDATE).len(), 1);

    // Ids that are not Bedrock emote ids are only relayed to Java players.
    let mut custom = dance.clone();
    custom.id = "my_custom_emote".into();
    let out = b.custom_payload(JAVA_A, p::CH_PLAY_EMOTE, &custom.encode().unwrap(), 0);
    assert!(bedrock_emotes(&out).is_empty());
    assert_eq!(payloads(&out, JAVA_B, p::CH_EMOTE_UPDATE).len(), 1);

    // Stopping an emote has no Bedrock equivalent.
    let stop = PlayEmote {
        id: STOP_EMOTE_ID.into(),
        name: String::new(),
        animation_name: String::new(),
        animation: String::new(),
        duration: 0.0,
    };
    let out = b.custom_payload(JAVA_A, p::CH_PLAY_EMOTE, &stop.encode().unwrap(), 0);
    assert!(bedrock_emotes(&out).is_empty());

    // A Bedrock player playing the same emote sends Java players the animation too.
    let out = b.bedrock_emote(BEDROCK, &id.to_uppercase(), 40);
    let e = EmoteUpdate::decode(payloads(&out, JAVA_B, p::CH_EMOTE_UPDATE)[0]).unwrap();
    assert_eq!((e.uuid, e.emote.id.as_str()), (BEDROCK, id));
    assert_eq!(e.emote.animation, dance.animation);
    assert!((e.emote.duration - 2.0).abs() < 1e-6);
}

#[test]
fn raw_bedrock_emote_packet_reaches_modded_java() {
    let mut b = server();
    let id = "4c8ae710-df2e-47cd-814d-cc7bf21a3d67";
    // runtime id 300 (VarULong), id, 60 ticks, xuid, platform id, flags
    let mut raw = vec![0xAC, 0x02];
    raw.push(id.len() as u8);
    raw.extend_from_slice(id.as_bytes());
    raw.push(60);
    raw.extend_from_slice(&[3, b'1', b'2', b'3', 0, 0]);
    let out = b.bedrock_packet(BEDROCK, 138, &raw, 0);
    let e = EmoteUpdate::decode(payloads(&out, JAVA_A, p::CH_EMOTE_UPDATE)[0]).unwrap();
    assert_eq!((e.uuid, e.emote.id.as_str()), (BEDROCK, id));
    assert!((e.emote.duration - 3.0).abs() < 1e-6);

    // A damaged packet is logged and nothing is sent.
    let out = b.bedrock_packet(BEDROCK, 138, &raw[..5], 0);
    assert!(!out.is_empty() && out.iter().all(|o| matches!(o, Out::Log(..))));
}
