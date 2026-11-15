//! Bedrock Skins Bridge: a Pumpkin plugin that is the server side of the Bedrock Skins mod
//! (custom geometry skins, capes, Persona cosmetics, emotes) and carries skins between Java and
//! Bedrock players.
//!
//! This file is only glue between Pumpkin's plugin API and `bedrock_skins_core::bridge::Bridge`,
//! which holds all the logic and is tested natively in `core/`.

use bedrock_skins_core::bridge::{Bridge, Level, Out, Platform, Position};
use bedrock_skins_core::config::{Config, DEFAULT_FILE};
use bedrock_skins_core::proto::bedrock_skin::{
    BedrockSkin, PersonaPiece as CorePiece, TintColor,
};
use bedrock_skins_core::proto::buf::Uuid;
use pumpkin_plugin_api::bedrock_packets::{
    BuildPlatform, CEmote, CPlayerList, CPlayerSkin, ClientboundPacket as BedrockPacket, PersonaPiece, PieceTintColor, PlayerListEntry, Skin,
};
use pumpkin_plugin_api::events::{
    BedrockServerboundPacket, EventHandler, EventPriority, PacketReceivedEvent, ServerboundPacket,
    player::{
        PlayerChangedWorldEvent, PlayerCustomPayloadEvent, PlayerJoinEvent, PlayerLeaveEvent,
        PlayerRegisterChannelEvent, PlayerRespawnEvent,
    },
};
use pumpkin_plugin_api::events_wit::{
    PacketReceivedEventData, PlayerChangedWorldEventData, PlayerCustomPayloadEventData, PlayerJoinEventData,
    PlayerLeaveEventData, PlayerRegisterChannelEventData, PlayerRespawnEventData,
};
use pumpkin_plugin_api::logging::{LogLevel, log};
use pumpkin_plugin_api::scheduler::SchedulerExt;
use pumpkin_plugin_api::{Context, Player, Plugin, PluginMetadata, Server, permissions, register_plugin};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Instant;

/// How often (server ticks) player positions are checked for Bedrock re-sends.
const TICK_PERIOD: u64 = 10;
const TAG: &str = "[BedrockSkinsBridge]";

static BRIDGE: Mutex<Option<Bridge>> = Mutex::new(None);
static START: OnceLock<Instant> = OnceLock::new();

fn bridge() -> MutexGuard<'static, Option<Bridge>> {
    BRIDGE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

fn uuid_of(player: &Player) -> Uuid {
    let id = player.get_id();
    Uuid {
        high: id.high,
        low: id.low,
    }
}

fn wit_uuid(u: Uuid) -> pumpkin_plugin_api::uuid::Uuid {
    pumpkin_plugin_api::uuid::Uuid {
        high: u.high,
        low: u.low,
    }
}

/// Runs `f` on the bridge and carries out what it returns.
fn with_bridge(server: &Server, f: impl FnOnce(&mut Bridge) -> Vec<Out>) {
    let (outs, debug) = {
        let mut guard = bridge();
        let Some(b) = guard.as_mut() else { return };
        (f(b), b.config.debug)
    };
    // The lock is released before calling back into the server.
    perform(server, outs, debug);
}

fn perform(server: &Server, outs: Vec<Out>, debug: bool) {
    for out in outs {
        match out {
            Out::Payload { to, channel, data } => {
                if let Some(java) = server.get_player_by_uuid(wit_uuid(to)).and_then(|p| p.as_java()) {
                    java.send_custom_payload(channel, data.as_slice());
                }
            }
            Out::BedrockSkin { to, owner, skin } => send_bedrock_skin(server, to, owner, &skin),
            Out::BedrockEmote {
                to,
                owner,
                emote_id,
                length_ticks,
            } => send_bedrock_emote(server, to, owner, &emote_id, length_ticks),
            Out::Log(level, msg) => {
                let level = match level {
                    Level::Debug if !debug => continue,
                    Level::Debug | Level::Info => LogLevel::Info,
                    Level::Warn => LogLevel::Warn,
                };
                log(level, &format!("{TAG} {msg}"));
            }
        }
    }
}

/// Shows `owner` to Bedrock player `to` with `skin`.
///
/// Two packets are sent: a player-list entry, which sets the skin the client uses the next time
/// it spawns `owner`, and a `PlayerSkin` packet, which is how Bedrock changes the skin of a
/// player that is already visible (and how third-party servers send custom geometry).
/// Pumpkin's plugin host drops both unless `patches/` has been applied to it.
fn send_bedrock_skin(server: &Server, to: Uuid, owner: Uuid, skin: &BedrockSkin) {
    let Some(viewer) = server.get_player_by_uuid(wit_uuid(to)).and_then(|p| p.as_bedrock()) else {
        return;
    };
    let Some(owner_player) = server.get_player_by_uuid(wit_uuid(owner)) else {
        return;
    };
    let wit_skin = to_wit_skin(skin);
    let entry = PlayerListEntry {
        uuid: wit_uuid(owner),
        entity_unique_id: i64::from(owner_player.as_entity().get_id()),
        username: owner_player.get_name(),
        xuid: String::new(),
        platform_chat_id: String::new(),
        build_platform: BuildPlatform::Unknown,
        skin: wit_skin.clone(),
        is_teacher: false,
        is_host: false,
        is_sub_client: false,
        player_color: vec![0, 0, 0, 0],
    };
    viewer.send_packet(&BedrockPacket::CPlayerList(CPlayerList {
        action: 0,
        entries: vec![entry],
    }));
    viewer.send_packet(&BedrockPacket::CPlayerSkin(CPlayerSkin {
        uuid: wit_uuid(owner),
        skin: wit_skin,
        new_skin_name: skin.skin_id.clone(),
        old_skin_name: String::new(),
    }));
}

/// Makes `owner`'s model play the emote `emote_id` for Bedrock player `to`.
///
/// Bedrock clients identify emotes by piece UUID and only play ones they know, so an emote the
/// Bedrock player doesn't have does nothing. Pumpkin's plugin host drops this packet unless
/// `patches/` has been applied to it.
fn send_bedrock_emote(server: &Server, to: Uuid, owner: Uuid, emote_id: &str, length_ticks: u32) {
    let Some(viewer) = server.get_player_by_uuid(wit_uuid(to)).and_then(|p| p.as_bedrock()) else {
        return;
    };
    let Some(owner_player) = server.get_player_by_uuid(wit_uuid(owner)) else {
        return;
    };
    viewer.send_packet(&BedrockPacket::CEmote(CEmote {
        actor_runtime_id: i64::from(owner_player.as_entity().get_id()),
        emote_id: emote_id.to_owned(),
        emote_length_ticks: length_ticks.min(i32::MAX as u32) as i32,
        xuid: String::new(),
        platform_id: String::new(),
        // Started by the server, and no chat notification.
        flags: 0b11,
    }));
}

/// The inverse of `to_wit_skin`, for the skin a Bedrock client sent at login.
fn from_wit_skin(s: Skin) -> BedrockSkin {
    BedrockSkin {
        skin_id: s.skin_id,
        play_fab_id: s.play_fab_id,
        resource_patch: s.resource_patch,
        image_width: s.image_width.max(0) as u32,
        image_height: s.image_height.max(0) as u32,
        skin_data: s.skin_data,
        cape_width: s.cape_width.max(0) as u32,
        cape_height: s.cape_height.max(0) as u32,
        cape_data: s.cape_data,
        geometry_data: s.geometry_data,
        geometry_data_engine_version: s.geometry_data_engine_version,
        animation_data: s.animation_data,
        cape_id: s.cape_id,
        full_id: s.full_id,
        arm_size: s.arm_size,
        skin_color: s.skin_color,
        persona_pieces: s
            .persona_pieces
            .into_iter()
            .map(|p| CorePiece {
                piece_id: p.piece_id,
                piece_type: p.piece_type,
                pack_id: p.pack_id,
                is_default: p.is_default,
                product_id: p.product_id,
            })
            .collect(),
        piece_tint_colors: s
            .piece_tint_colors
            .into_iter()
            .map(|t| TintColor {
                piece_type: t.piece_type,
                colors: t.colors,
            })
            .collect(),
        is_premium: s.is_premium,
        is_persona: s.is_persona,
        persona_cape_on_classic: s.persona_cape_on_classic,
        is_primary_user: s.is_primary_user,
        override_appearance: s.override_appearance,
        is_trusted: s.is_trusted,
    }
}

/// Registers a player with the bridge and, for Bedrock players, hands over the skin they logged
/// in with.
fn register_player(server: &Server, player: &Player) {
    let uuid = uuid_of(player);
    let name = player.get_name();
    let bedrock = player.as_bedrock();
    let platform = if bedrock.is_some() { Platform::Bedrock } else { Platform::Java };
    with_bridge(server, |b| b.join(uuid, &name, platform));
    if let Some(skin) = bedrock.and_then(|b| b.get_login_skin()) {
        let skin = from_wit_skin(skin);
        with_bridge(server, |b| b.bedrock_login_skin(uuid, skin));
    }
}

fn to_wit_skin(s: &BedrockSkin) -> Skin {
    Skin {
        skin_id: s.skin_id.clone(),
        play_fab_id: s.play_fab_id.clone(),
        resource_patch: s.resource_patch.clone(),
        image_width: s.image_width as i32,
        image_height: s.image_height as i32,
        skin_data: s.skin_data.clone(),
        animations: Vec::new(),
        cape_width: s.cape_width as i32,
        cape_height: s.cape_height as i32,
        cape_data: s.cape_data.clone(),
        geometry_data: s.geometry_data.clone(),
        animation_data: s.animation_data.clone(),
        geometry_data_engine_version: s.geometry_data_engine_version.clone(),
        cape_id: s.cape_id.clone(),
        full_id: s.full_id.clone(),
        arm_size: s.arm_size.clone(),
        skin_color: s.skin_color.clone(),
        persona_pieces: s
            .persona_pieces
            .iter()
            .map(|p| PersonaPiece {
                is_default: p.is_default,
                pack_id: p.pack_id.clone(),
                piece_id: p.piece_id.clone(),
                piece_type: p.piece_type.clone(),
                product_id: p.product_id.clone(),
            })
            .collect(),
        piece_tint_colors: s
            .piece_tint_colors
            .iter()
            .map(|t| PieceTintColor {
                piece_type: t.piece_type.clone(),
                colors: t.colors.clone(),
            })
            .collect(),
        is_premium: s.is_premium,
        is_persona: s.is_persona,
        persona_cape_on_classic: s.persona_cape_on_classic,
        is_primary_user: s.is_primary_user,
        override_appearance: s.override_appearance,
        is_trusted: s.is_trusted,
        profile_hash: String::new(),
    }
}

// ---------------------------------------------------------------------------------------------
// Event handlers

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, server: Server, e: PlayerJoinEventData) -> PlayerJoinEventData {
        register_player(&server, &e.player);
        e
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, e: PlayerLeaveEventData) -> PlayerLeaveEventData {
        let uuid = uuid_of(&e.player);
        if let Some(b) = bridge().as_mut() {
            b.leave(uuid);
        }
        e
    }
}

struct OnRegisterChannel;
impl EventHandler<PlayerRegisterChannelEvent> for OnRegisterChannel {
    fn handle(&self, server: Server, e: PlayerRegisterChannelEventData) -> PlayerRegisterChannelEventData {
        if e.channel.starts_with("bedrockskins:") {
            let uuid = uuid_of(&e.player);
            with_bridge(&server, |b| b.channel_registered(uuid, &e.channel));
        }
        e
    }
}

struct OnCustomPayload;
impl EventHandler<PlayerCustomPayloadEvent> for OnCustomPayload {
    fn handle(&self, server: Server, e: PlayerCustomPayloadEventData) -> PlayerCustomPayloadEventData {
        if e.channel.starts_with("bedrockskins:") {
            let uuid = uuid_of(&e.player);
            let now = now_ms();
            with_bridge(&server, |b| b.custom_payload(uuid, &e.channel, &e.data, now));
        }
        e
    }
}

/// Bedrock skin changes and emotes. Sees every play packet, so it returns fast for the rest.
struct OnPacket;
impl EventHandler<PacketReceivedEvent> for OnPacket {
    fn handle(&self, server: Server, e: PacketReceivedEventData) -> PacketReceivedEventData {
        match &e.packet {
            ServerboundPacket::Bedrock(BedrockServerboundPacket::SEmote(emote)) => {
                let uuid = uuid_of(&e.player);
                with_bridge(&server, |b| {
                    b.bedrock_emote(uuid, &emote.emote_id, emote.emote_length_ticks)
                });
            }
            ServerboundPacket::Java(_) => {}
            _ if matches!(
                e.packet_id & 0x3FF,
                bedrock_skins_core::proto::bedrock_skin::PLAYER_SKIN_PACKET_ID
                    | bedrock_skins_core::proto::bedrock_skin::EMOTE_PACKET_ID
            ) =>
            {
                if e.player.as_bedrock().is_some() {
                    let uuid = uuid_of(&e.player);
                    let now = now_ms();
                    with_bridge(&server, |b| b.bedrock_packet(uuid, e.packet_id, &e.raw_payload, now));
                }
            }
            _ => {}
        }
        e
    }
}

/// Pumpkin re-spawns a player for everyone on respawn and world change, re-sending its own idea
/// of their skin to Bedrock viewers, so ours is sent again afterwards.
struct OnRespawn;
impl EventHandler<PlayerRespawnEvent> for OnRespawn {
    fn handle(&self, _server: Server, e: PlayerRespawnEventData) -> PlayerRespawnEventData {
        let uuid = uuid_of(&e.player);
        if let Some(b) = bridge().as_mut() {
            b.forget_tracking(uuid);
        }
        e
    }
}

struct OnChangedWorld;
impl EventHandler<PlayerChangedWorldEvent> for OnChangedWorld {
    fn handle(&self, _server: Server, e: PlayerChangedWorldEventData) -> PlayerChangedWorldEventData {
        let uuid = uuid_of(&e.player);
        if let Some(b) = bridge().as_mut() {
            b.forget_tracking(uuid);
        }
        e
    }
}

fn positions(server: &Server) -> Vec<Position> {
    server
        .get_all_players()
        .iter()
        .map(|p| {
            let (x, y, z) = p.get_position();
            Position {
                uuid: uuid_of(p),
                world: p.get_world().get_id(),
                x,
                y,
                z,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Plugin

struct BedrockSkinsBridge;

fn load_config(folder: &str) -> Config {
    let path = format!("{folder}/config.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            let _ = std::fs::create_dir_all(folder);
            if std::fs::write(&path, DEFAULT_FILE).is_ok() {
                log(LogLevel::Info, &format!("{TAG} wrote default settings to {path}"));
            }
            DEFAULT_FILE.to_owned()
        }
    };
    let (config, problems) = Config::parse(&text);
    for p in problems {
        log(LogLevel::Warn, &format!("{TAG} config.toml {p}"));
    }
    config
}

impl Plugin for BedrockSkinsBridge {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "bedrock-skins-bridge".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Bedrock Skins Bridge contributors".into()],
            description: "Server side of the Bedrock Skins mod, with Java <-> Bedrock skin bridging.".into(),
            dependencies: vec![],
            permissions: vec![permissions::FS_READ_DATA.into(), permissions::FS_WRITE_DATA.into()],
        }
    }

    fn on_load(&self, context: Context) -> pumpkin_plugin_api::Result<()> {
        START.get_or_init(Instant::now);
        let config = load_config(&context.get_data_folder());
        let capture = config.bedrock_capture;
        let sync = config.bedrock_sync;
        *bridge() = Some(Bridge::new(config));

        context.register_event_handler::<PlayerJoinEvent, _>(OnJoin, EventPriority::Normal, false)?;
        context.register_event_handler::<PlayerLeaveEvent, _>(OnLeave, EventPriority::Normal, false)?;
        context.register_event_handler::<PlayerRegisterChannelEvent, _>(
            OnRegisterChannel,
            EventPriority::Normal,
            false,
        )?;
        context.register_event_handler::<PlayerCustomPayloadEvent, _>(OnCustomPayload, EventPriority::Normal, false)?;
        if capture {
            context.register_event_handler::<PacketReceivedEvent, _>(OnPacket, EventPriority::Normal, false)?;
        }
        if sync {
            context.register_event_handler::<PlayerRespawnEvent, _>(OnRespawn, EventPriority::Normal, false)?;
            context.register_event_handler::<PlayerChangedWorldEvent, _>(
                OnChangedWorld,
                EventPriority::Normal,
                false,
            )?;
            context.schedule_repeating_task(TICK_PERIOD, TICK_PERIOD, |server| {
                let pos = positions(&server);
                with_bridge(&server, |b| b.tick(TICK_PERIOD, &pos));
            });
        }

        // Players already online (plugin reload) are picked up as if they had just joined.
        // Their clients registered channels before we were listening, so treat them as modded
        // only once they use a channel.
        let server = context.get_server();
        for p in server.get_all_players() {
            register_player(&server, &p);
        }

        log(
            LogLevel::Info,
            &format!(
                "{TAG} v{} loaded (Bedrock sync {}, Bedrock capture {})",
                env!("CARGO_PKG_VERSION"),
                if sync { "on" } else { "off" },
                if capture { "on" } else { "off" }
            ),
        );
        Ok(())
    }

    fn on_unload(&self, _context: Context) -> pumpkin_plugin_api::Result<()> {
        *bridge() = None;
        Ok(())
    }
}

register_plugin!(BedrockSkinsBridge);
