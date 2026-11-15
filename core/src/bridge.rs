//! The plugin's logic, free of any Pumpkin types: events go in, a list of things to send comes
//! out. The wasm glue (`plugin.rs`) only translates between this and the Pumpkin API, so every
//! rule here is covered by native tests.
//!
//! The Java side mirrors the mod's own server code (`BedrockSkins.java`, `ServerSkinHandler`):
//! a skin change is broadcast as a lightweight `skin_announce`; clients that lack the content ask
//! for it with `request_skin_data` and get a `skin_update`.

use crate::config::Config;
use crate::proto::bedrock_skin::{
    BedrockSkin, EMOTE_PACKET_ID, PLAYER_SKIN_PACKET_ID, parse_emote_packet, parse_player_skin_packet,
};
use crate::proto::buf::{DecodeError, Uuid};
use crate::proto::payloads::{
    self as p, CosmeticsUpdate, EmoteUpdate, PlayEmote, RequestSkinData, SetCosmetics, SetSkin, SkinAnnounce,
    SkinUpdate,
};
use crate::translate::{self, ModSkin};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

pub const REGISTER_CHANNEL: &str = "minecraft:register";

/// Mod limits (`ServerSkinHandler`).
const MAX_SKIN_TEXTURE: usize = 512 * 1024;
const MAX_CAPE: usize = 256 * 1024;
const MAX_COSMETIC_GEOMETRY_CHARS: usize = 150_000;
const MAX_EMOTE_ANIMATION_CHARS: usize = 150_000;
const MAX_EMOTE_SECONDS: f32 = 30.0;
/// Most distinct emotes remembered for Bedrock-triggered playback.
const MAX_REMEMBERED_EMOTES: usize = 256;

/// Whether `id` looks like a Bedrock emote piece id (a UUID), the only kind a Bedrock client can play.
fn is_emote_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}
/// Ticks between a Bedrock viewer coming into range of a player and the skin being re-sent, so
/// it lands after Pumpkin's own spawn packets (which carry Pumpkin's idea of the skin).
pub const RESEND_DELAY_TICKS: u64 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Java,
    Bedrock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
}

/// Something for the glue to do.
#[derive(Debug, Clone)]
pub enum Out {
    /// A custom payload to a Java player.
    Payload {
        to: Uuid,
        channel: &'static str,
        data: Arc<Vec<u8>>,
    },
    /// Show `owner` to Bedrock player `to` with this skin (via a player-list entry).
    BedrockSkin {
        to: Uuid,
        owner: Uuid,
        skin: Arc<BedrockSkin>,
    },
    /// Play the emote `emote_id` (a Bedrock emote piece UUID) on `owner`'s model for Bedrock player `to`.
    BedrockEmote {
        to: Uuid,
        owner: Uuid,
        emote_id: String,
        length_ticks: u32,
    },
    Log(Level, String),
}

/// A player's position, for the Bedrock re-send check.
#[derive(Debug, Clone)]
pub struct Position {
    pub uuid: Uuid,
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug)]
struct PlayerState {
    name: String,
    platform: Platform,
    /// The client has the mod (registered one of its channels or used one).
    modded: bool,
}

#[derive(Debug, Clone)]
struct Active {
    skin_id: String,
    hash: String,
}

#[derive(Debug, Default)]
pub struct Bridge {
    pub config: Config,
    players: BTreeMap<Uuid, PlayerState>,
    /// Skin content by hash, shared by every player wearing it.
    skins: HashMap<String, Arc<ModSkin>>,
    active: BTreeMap<Uuid, Active>,
    /// Encoded cosmetics list (everything after the UUID in `cosmetics_update`).
    cosmetics: BTreeMap<Uuid, Arc<Vec<u8>>>,
    last_skin_change_ms: HashMap<Uuid, u64>,
    /// What Bedrock players should see for a player whose skin came through this plugin.
    bedrock_view: BTreeMap<Uuid, Arc<BedrockSkin>>,
    /// Mod skins already translated for Bedrock (`None` = translation failed), by hash.
    bedrock_cache: HashMap<String, Option<Arc<BedrockSkin>>>,
    /// Emotes Java players have played, by lowercase id, so a Bedrock player playing the same
    /// emote can show it to Java players who don't have it installed.
    emotes: HashMap<String, PlayEmote>,
    /// (viewer, owner) pairs currently within the re-send radius.
    in_range: BTreeSet<(Uuid, Uuid)>,
    /// Re-sends waiting for their tick: (due tick, viewer, owner).
    pending: Vec<(u64, Uuid, Uuid)>,
    tick: u64,
}

fn log(out: &mut Vec<Out>, level: Level, msg: impl Into<String>) {
    out.push(Out::Log(level, msg.into()));
}

impl Bridge {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    fn name(&self, uuid: Uuid) -> String {
        self.players
            .get(&uuid)
            .map_or_else(|| uuid.to_string(), |p| p.name.clone())
    }

    fn java_receivers(&self) -> impl Iterator<Item = Uuid> + '_ {
        let all = self.config.send_to_unregistered_clients;
        self.players
            .iter()
            .filter(move |(_, s)| s.platform == Platform::Java && (s.modded || all))
            .map(|(u, _)| *u)
    }

    fn bedrock_players(&self) -> impl Iterator<Item = Uuid> + '_ {
        self.players
            .iter()
            .filter(|(_, s)| s.platform == Platform::Bedrock)
            .map(|(u, _)| *u)
    }

    fn broadcast(&self, out: &mut Vec<Out>, channel: &'static str, data: Vec<u8>) {
        let data = Arc::new(data);
        for to in self.java_receivers() {
            out.push(Out::Payload {
                to,
                channel,
                data: data.clone(),
            });
        }
    }

    fn mark_modded(&mut self, uuid: Uuid, out: &mut Vec<Out>) {
        let Some(state) = self.players.get_mut(&uuid) else {
            return;
        };
        if state.modded || state.platform != Platform::Java {
            return;
        }
        state.modded = true;
        log(out, Level::Debug, format!("{} has the Bedrock Skins mod", state.name));
        // Catch the newcomer up, as the mod's server does on join.
        for (owner, active) in &self.active {
            let a = SkinAnnounce {
                uuid: *owner,
                skin_id: Some(active.skin_id.clone()),
                hash: active.hash.clone(),
            };
            out.push(Out::Payload {
                to: uuid,
                channel: p::CH_SKIN_ANNOUNCE,
                data: Arc::new(a.encode()),
            });
        }
        for (owner, list) in &self.cosmetics {
            let mut data = Vec::with_capacity(16 + list.len());
            data.extend_from_slice(&owner.high.to_be_bytes());
            data.extend_from_slice(&owner.low.to_be_bytes());
            data.extend_from_slice(list);
            out.push(Out::Payload {
                to: uuid,
                channel: p::CH_COSMETICS_UPDATE,
                data: Arc::new(data),
            });
        }
    }

    // -----------------------------------------------------------------------------------------
    // Connection lifecycle

    pub fn join(&mut self, uuid: Uuid, name: &str, platform: Platform) -> Vec<Out> {
        let mut out = Vec::new();
        self.players.insert(
            uuid,
            PlayerState {
                name: name.to_owned(),
                platform,
                modded: false,
            },
        );
        if platform == Platform::Java {
            // `minecraft:register` lists the channels the sender can receive. Without it the mod's
            // client refuses to send on them.
            let data = Arc::new(p::register_payload(&p::SERVERBOUND_CHANNELS));
            out.push(Out::Payload {
                to: uuid,
                channel: REGISTER_CHANNEL,
                data,
            });
            if self.config.send_to_unregistered_clients {
                self.mark_modded(uuid, &mut out);
            }
        }
        out
    }

    pub fn leave(&mut self, uuid: Uuid) {
        self.players.remove(&uuid);
        self.active.remove(&uuid);
        self.cosmetics.remove(&uuid);
        self.last_skin_change_ms.remove(&uuid);
        self.bedrock_view.remove(&uuid);
        self.forget_tracking(uuid);
        self.clean_unused_skins();
    }

    /// The player was re-spawned for everyone (respawn, world change): Pumpkin re-sends its own
    /// skin with the spawn, so ours must be sent again.
    pub fn forget_tracking(&mut self, uuid: Uuid) {
        self.in_range.retain(|(v, o)| *v != uuid && *o != uuid);
        self.pending.retain(|(_, v, o)| *v != uuid && *o != uuid);
    }

    pub fn channel_registered(&mut self, uuid: Uuid, channel: &str) -> Vec<Out> {
        let mut out = Vec::new();
        if p::CLIENTBOUND_CHANNELS.contains(&channel) {
            self.mark_modded(uuid, &mut out);
        }
        out
    }

    fn clean_unused_skins(&mut self) {
        let used: BTreeSet<&String> = self.active.values().map(|a| &a.hash).collect();
        self.skins.retain(|h, _| used.contains(h));
        self.bedrock_cache.retain(|h, _| used.contains(h));
    }

    // -----------------------------------------------------------------------------------------
    // Java (mod) payloads

    pub fn custom_payload(&mut self, from: Uuid, channel: &str, data: &[u8], now_ms: u64) -> Vec<Out> {
        let mut out = Vec::new();
        if !channel.starts_with("bedrockskins:") || !self.players.contains_key(&from) {
            return out;
        }
        self.mark_modded(from, &mut out);
        let who = self.name(from);
        let result: Result<(), DecodeError> = (|| {
            match channel {
                p::CH_SET_SKIN => self.on_set_skin(from, SetSkin::decode(data)?, now_ms, &mut out),
                p::CH_REQUEST_SKIN_DATA => self.on_request(from, &RequestSkinData::decode(data)?, &mut out),
                p::CH_SET_COSMETICS => self.on_set_cosmetics(from, SetCosmetics::decode(data)?, &mut out),
                p::CH_PLAY_EMOTE => self.on_play_emote(from, PlayEmote::decode(data)?, &mut out),
                _ => {}
            }
            Ok(())
        })();
        if let Err(e) = result {
            log(
                &mut out,
                Level::Warn,
                format!("{who} sent a malformed {channel} payload: {e}"),
            );
        }
        out
    }

    fn on_set_skin(&mut self, from: Uuid, msg: SetSkin, now_ms: u64, out: &mut Vec<Out>) {
        let who = self.name(from);
        if let Some(&last) = self.last_skin_change_ms.get(&from)
            && now_ms.saturating_sub(last) < self.config.skin_change_cooldown_ms
        {
            log(out, Level::Warn, format!("{who} is changing skins too quickly"));
            return;
        }
        let Some(skin_id) = msg.skin_id else {
            self.last_skin_change_ms.insert(from, now_ms);
            log(out, Level::Info, format!("{who} reset their skin"));
            self.active.remove(&from);
            let had_bedrock_view = self.bedrock_view.remove(&from).is_some();
            self.clean_unused_skins();
            self.broadcast(
                out,
                p::CH_SKIN_ANNOUNCE,
                SkinAnnounce {
                    uuid: from,
                    skin_id: None,
                    hash: String::new(),
                }
                .encode(),
            );
            if had_bedrock_view {
                log(
                    out,
                    Level::Debug,
                    format!("Bedrock players keep seeing {who}'s last skin until they see them respawn"),
                );
            }
            return;
        };
        if msg.texture.len() > MAX_SKIN_TEXTURE {
            log(
                out,
                Level::Warn,
                format!("{who} sent an oversized texture ({} bytes)", msg.texture.len()),
            );
            return;
        }
        if !crate::codec::png::has_png_signature(&msg.texture) {
            log(
                out,
                Level::Warn,
                format!("{who} sent an invalid texture format (not PNG)"),
            );
            return;
        }
        if msg.cape.len() > MAX_CAPE || (!msg.cape.is_empty() && !crate::codec::png::has_png_signature(&msg.cape)) {
            log(out, Level::Warn, format!("{who} sent an invalid cape texture"));
            return;
        }
        log(out, Level::Info, format!("{who} set skin to {skin_id}"));
        self.last_skin_change_ms.insert(from, now_ms);
        let skin = ModSkin {
            skin_id,
            geometry: msg.geometry,
            texture: msg.texture,
            cape: msg.cape,
        };
        self.apply_skin(from, skin, None, out);
    }

    /// Makes `skin` the player's active skin: announce to modded Java clients, show to Bedrock.
    /// `bedrock` is the original Bedrock skin when the change came from a Bedrock client.
    fn apply_skin(&mut self, owner: Uuid, skin: ModSkin, bedrock: Option<Arc<BedrockSkin>>, out: &mut Vec<Out>) {
        let hash = p::compute_hash(&skin.geometry, &skin.texture, &skin.cape);
        let skin_id = skin.skin_id.clone();
        self.skins.entry(hash.clone()).or_insert_with(|| Arc::new(skin));
        self.active.insert(
            owner,
            Active {
                skin_id: skin_id.clone(),
                hash: hash.clone(),
            },
        );
        self.clean_unused_skins();
        self.broadcast(
            out,
            p::CH_SKIN_ANNOUNCE,
            SkinAnnounce {
                uuid: owner,
                skin_id: Some(skin_id),
                hash: hash.clone(),
            }
            .encode(),
        );

        if !self.config.bedrock_sync {
            return;
        }
        let view = match bedrock {
            Some(b) => Some(b),
            None => self.bedrock_skin_for(&hash, out),
        };
        let Some(view) = view else { return };
        self.bedrock_view.insert(owner, view.clone());
        let viewers: Vec<Uuid> = self.bedrock_players().filter(|v| *v != owner).collect();
        for to in viewers {
            out.push(Out::BedrockSkin {
                to,
                owner,
                skin: view.clone(),
            });
        }
    }

    fn bedrock_skin_for(&mut self, hash: &str, out: &mut Vec<Out>) -> Option<Arc<BedrockSkin>> {
        if let Some(cached) = self.bedrock_cache.get(hash) {
            return cached.clone();
        }
        let skin = self.skins.get(hash)?.clone();
        let result = match translate::mod_to_bedrock(&skin, hash) {
            Ok(b) => Some(Arc::new(b)),
            Err(e) => {
                log(
                    out,
                    Level::Warn,
                    format!("cannot show skin {} to Bedrock players: {e}", skin.skin_id),
                );
                None
            }
        };
        self.bedrock_cache.insert(hash.to_owned(), result.clone());
        result
    }

    fn on_request(&mut self, from: Uuid, msg: &RequestSkinData, out: &mut Vec<Out>) {
        let Some(skin) = self.skins.get(&msg.hash) else {
            log(
                out,
                Level::Debug,
                format!("{} requested unknown skin hash {}", self.name(from), msg.hash),
            );
            return;
        };
        let owner = self
            .active
            .iter()
            .find(|(_, a)| a.hash == msg.hash)
            .map_or(from, |(u, _)| *u);
        let update = SkinUpdate {
            uuid: owner,
            skin_id: Some(skin.skin_id.clone()),
            geometry: skin.geometry.clone(),
            texture: skin.texture.clone(),
            cape: skin.cape.clone(),
        };
        match update.encode() {
            Ok(data) => out.push(Out::Payload {
                to: from,
                channel: p::CH_SKIN_UPDATE,
                data: Arc::new(data),
            }),
            Err(e) => log(out, Level::Warn, format!("cannot send skin {}: {e}", skin.skin_id)),
        }
    }

    fn on_set_cosmetics(&mut self, from: Uuid, msg: SetCosmetics, out: &mut Vec<Out>) {
        for c in &msg.cosmetics {
            let bad = c.id.trim().is_empty()
                || c.kind.trim().is_empty()
                || c.geometry.encode_utf16().count() > MAX_COSMETIC_GEOMETRY_CHARS
                || !translate::is_valid_cosmetic_texture(&c.texture);
            if bad {
                log(
                    out,
                    Level::Warn,
                    format!("{} sent invalid Persona cosmetic data", self.name(from)),
                );
                return;
            }
        }
        let list = match (SetCosmetics {
            cosmetics: msg.cosmetics.clone(),
        })
        .encode()
        {
            Ok(l) => l,
            Err(e) => {
                log(out, Level::Warn, format!("cannot relay cosmetics: {e}"));
                return;
            }
        };
        if msg.cosmetics.is_empty() {
            self.cosmetics.remove(&from);
        } else {
            self.cosmetics.insert(from, Arc::new(list));
        }
        match (CosmeticsUpdate {
            uuid: from,
            cosmetics: msg.cosmetics,
        })
        .encode()
        {
            Ok(data) => self.broadcast(out, p::CH_COSMETICS_UPDATE, data),
            Err(e) => log(out, Level::Warn, format!("cannot relay cosmetics: {e}")),
        }
    }

    fn on_play_emote(&mut self, from: Uuid, emote: PlayEmote, out: &mut Vec<Out>) {
        let valid = !emote.id.trim().is_empty()
            && !emote.animation.trim().is_empty()
            && emote.animation.encode_utf16().count() <= MAX_EMOTE_ANIMATION_CHARS
            && emote.duration > 0.0
            && emote.duration <= MAX_EMOTE_SECONDS;
        if !emote.is_stop() && !valid {
            return;
        }
        if !emote.is_stop() {
            let key = emote.id.to_ascii_lowercase();
            if self.emotes.len() < MAX_REMEMBERED_EMOTES || self.emotes.contains_key(&key) {
                self.emotes.insert(key, emote.clone());
            }
            // Bedrock clients play an emote from its id alone, if they know the emote.
            if self.config.bedrock_sync && is_emote_uuid(&emote.id) {
                let length_ticks = (emote.duration * 20.0).round() as u32;
                let viewers: Vec<Uuid> = self.bedrock_players().filter(|v| *v != from).collect();
                let who = self.name(from);
                log(
                    out,
                    Level::Debug,
                    format!("{who} played emote {}, sending it to {} Bedrock player(s)", emote.id, viewers.len()),
                );
                for to in viewers {
                    out.push(Out::BedrockEmote {
                        to,
                        owner: from,
                        emote_id: emote.id.clone(),
                        length_ticks,
                    });
                }
            } else if self.config.bedrock_sync {
                log(
                    out,
                    Level::Debug,
                    format!("emote id {:?} is not a Bedrock emote id, not sending it to Bedrock players", emote.id),
                );
            }
        }
        match (EmoteUpdate { uuid: from, emote }).encode() {
            Ok(data) => self.broadcast(out, p::CH_EMOTE_UPDATE, data),
            Err(e) => log(out, Level::Warn, format!("cannot relay emote: {e}")),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Bedrock packets

    /// A raw serverbound Bedrock packet. Only PlayerSkin (93) and Emote (138) are of interest;
    /// Pumpkin's plugin host hands both over as raw bytes.
    pub fn bedrock_packet(&mut self, from: Uuid, packet_id: i32, payload: &[u8], now_ms: u64) -> Vec<Out> {
        let mut out = Vec::new();
        if packet_id & 0x3FF == EMOTE_PACKET_ID {
            return match parse_emote_packet(payload) {
                Ok(e) => self.bedrock_emote(from, &e.emote_id, e.length_ticks.min(i32::MAX as u32) as i32),
                Err(e) => {
                    let who = self.name(from);
                    log(&mut out, Level::Warn, format!("could not read {who}'s Bedrock emote: {e}"));
                    out
                }
            };
        }
        if packet_id & 0x3FF != PLAYER_SKIN_PACKET_ID {
            return out;
        }
        let who = self.name(from);
        if let Some(&last) = self.last_skin_change_ms.get(&from)
            && now_ms.saturating_sub(last) < self.config.skin_change_cooldown_ms
        {
            log(&mut out, Level::Warn, format!("{who} is changing skins too quickly"));
            return out;
        }
        let skin = match parse_player_skin_packet(payload) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                log(
                    &mut out,
                    Level::Warn,
                    format!("could not read {who}'s Bedrock skin change: {e}"),
                );
                return out;
            }
        };
        self.last_skin_change_ms.insert(from, now_ms);
        log(
            &mut out,
            Level::Info,
            format!("{who} changed their Bedrock skin ({})", skin.full_id),
        );
        self.accept_bedrock_skin(from, skin, &mut out);
        out
    }

    /// The skin a Bedrock client sent when it logged in (Pumpkin keeps it in the login data and
    /// the plugin API hands it over). Treated like a skin change, without the rate limit, so
    /// Java players and other Bedrock players see it from the start.
    pub fn bedrock_login_skin(&mut self, from: Uuid, skin: BedrockSkin) -> Vec<Out> {
        let mut out = Vec::new();
        if !self.config.bedrock_capture {
            return out;
        }
        let who = self.name(from);
        if !skin.images_consistent() {
            log(
                &mut out,
                Level::Warn,
                format!("{who}'s Bedrock login skin has inconsistent image sizes, ignoring it"),
            );
            return out;
        }
        log(
            &mut out,
            Level::Info,
            format!(
                "{who}'s Bedrock login skin: {} ({}x{}, {} byte geometry)",
                skin.skin_id,
                skin.image_width,
                skin.image_height,
                skin.geometry_data.len()
            ),
        );
        self.accept_bedrock_skin(from, Arc::new(skin), &mut out);
        out
    }

    /// Makes a Bedrock player's skin their active skin: translated for Java players where
    /// possible, passed through to the other Bedrock players.
    fn accept_bedrock_skin(&mut self, from: Uuid, skin: Arc<BedrockSkin>, out: &mut Vec<Out>) {
        let who = self.name(from);
        match translate::bedrock_to_mod(&skin) {
            Ok(mod_skin) => self.apply_skin(from, mod_skin, Some(skin), out),
            Err(e) => {
                log(
                    out,
                    Level::Warn,
                    format!("cannot show {who}'s Bedrock skin to Java players: {e}"),
                );
                if self.config.bedrock_sync {
                    self.bedrock_view.insert(from, skin.clone());
                    let viewers: Vec<Uuid> = self.bedrock_players().filter(|v| *v != from).collect();
                    for to in viewers {
                        out.push(Out::BedrockSkin {
                            to,
                            owner: from,
                            skin: skin.clone(),
                        });
                    }
                }
            }
        }
    }

    /// A Bedrock emote: forwarded to modded Java clients with only its piece id, which the mod
    /// resolves against the emotes it has installed.
    pub fn bedrock_emote(&mut self, from: Uuid, emote_id: &str, length_ticks: i32) -> Vec<Out> {
        let mut out = Vec::new();
        if emote_id.is_empty() || emote_id.len() > 128 {
            return out;
        }
        let seconds = (length_ticks.max(0) as f32 / 20.0).min(MAX_EMOTE_SECONDS);
        // A Java player played this emote before: send its animation along, so Java players who
        // don't have the emote installed can play it too.
        let known = self.emotes.get(&emote_id.to_ascii_lowercase()).cloned();
        let who = self.name(from);
        log(
            &mut out,
            Level::Debug,
            format!(
                "{who} played Bedrock emote {emote_id} ({})",
                if known.is_some() { "animation known" } else { "Java players need it installed" }
            ),
        );
        let emote = known.unwrap_or_else(|| PlayEmote {
            id: emote_id.to_owned(),
            name: String::new(),
            animation_name: String::new(),
            animation: String::new(),
            duration: seconds,
        });
        match (EmoteUpdate { uuid: from, emote }).encode() {
            Ok(data) => self.broadcast(&mut out, p::CH_EMOTE_UPDATE, data),
            Err(e) => log(&mut out, Level::Warn, format!("cannot relay Bedrock emote: {e}")),
        }
        out
    }

    // -----------------------------------------------------------------------------------------
    // Periodic Bedrock re-send

    /// Called every `ticks` server ticks with every online player's position.
    pub fn tick(&mut self, ticks: u64, positions: &[Position]) -> Vec<Out> {
        self.tick += ticks;
        let mut out = Vec::new();
        if !self.config.bedrock_sync || self.bedrock_view.is_empty() {
            self.in_range.clear();
            self.pending.clear();
            return out;
        }
        let r2 = self.config.bedrock_resend_radius * self.config.bedrock_resend_radius;
        let by_uuid: HashMap<Uuid, &Position> = positions.iter().map(|p| (p.uuid, p)).collect();
        let mut now_in = BTreeSet::new();
        for viewer in self.bedrock_players() {
            let Some(vp) = by_uuid.get(&viewer) else { continue };
            for owner in self.bedrock_view.keys() {
                if *owner == viewer {
                    continue;
                }
                let Some(op) = by_uuid.get(owner) else { continue };
                let (dx, dy, dz) = (vp.x - op.x, vp.y - op.y, vp.z - op.z);
                if vp.world == op.world && dx * dx + dy * dy + dz * dz <= r2 {
                    now_in.insert((viewer, *owner));
                }
            }
        }
        for pair in now_in.difference(&self.in_range) {
            self.pending.push((self.tick + RESEND_DELAY_TICKS, pair.0, pair.1));
        }
        self.in_range = now_in;
        let tick = self.tick;
        let (due, later): (Vec<_>, Vec<_>) = self.pending.drain(..).partition(|(t, _, _)| *t <= tick);
        self.pending = later;
        for (_, to, owner) in due {
            if self.in_range.contains(&(to, owner))
                && let Some(skin) = self.bedrock_view.get(&owner)
            {
                out.push(Out::BedrockSkin {
                    to,
                    owner,
                    skin: skin.clone(),
                });
            }
        }
        out
    }

    /// Short state summary for the status command.
    #[must_use]
    pub fn status(&self) -> Vec<String> {
        let java = self.players.values().filter(|p| p.platform == Platform::Java).count();
        let modded = self.players.values().filter(|p| p.modded).count();
        let bedrock = self
            .players
            .values()
            .filter(|p| p.platform == Platform::Bedrock)
            .count();
        let bytes: usize = self
            .skins
            .values()
            .map(|s| s.geometry.len() + s.texture.len() + s.cape.len())
            .sum();
        let mut lines = vec![
            format!("players: {java} Java ({modded} with the mod), {bedrock} Bedrock"),
            format!(
                "custom skins: {} active, {} stored ({} KiB)",
                self.active.len(),
                self.skins.len(),
                bytes / 1024
            ),
            format!(
                "cosmetics: {} players; Bedrock overrides: {}",
                self.cosmetics.len(),
                self.bedrock_view.len()
            ),
        ];
        for (u, a) in &self.active {
            lines.push(format!(
                "  {} -> {} ({})",
                self.name(*u),
                a.skin_id,
                &a.hash[..a.hash.len().min(12)]
            ));
        }
        lines
    }
}
