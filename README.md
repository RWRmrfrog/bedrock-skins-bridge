# Bedrock Skins Bridge

A [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) server plugin that lets Java and Bedrock players see each other's custom player models.

It is the server side of the [Bedrock Skins](https://github.com/BrandonItaly/Bedrock-Skins) Java mod, which lets Java players use Bedrock-format skins with custom 3D geometry, HD textures, capes and emotes. On its own, the mod only works locally for Java players who have it installed. This plugin carries those skins to other Java and Bedrock players, and Bedrock players' skins to Java players.

> Pumpkin's plugin API can't implement the skin functionality required by this plugin yet, so the plugin needs a **patched build of the Pumpkin server**. The patches are included and are small, but you do have to build Pumpkin yourself. See [Installation](#installation).

## AI Disclaimer

Claude was used heavily in the development of this plugin. This is my first experience coding in rust and I know little to nothing about the bedrock protocol. This project is no more than a prototype and shouldn't be used for important servers yet. If you feel you have more experience in rust and would like to make this plugin a bit more stable, feel free to fork this repository and create a pull request.

## Features

<img width="2280" height="1080" alt="Screenshot_20261114-234157_Minecraft" src="https://github.com/user-attachments/assets/4d00fa38-39b9-4f32-a414-94e24f6f2dc4" />

[Video Showcase](https://youtu.be/IS7opdeRGsQ)


| What | Status |
|---|---|
| Modded Java ↔ modded Java: custom geometry, HD textures, capes, live skin switching | Works |
| Modded Java ↔ modded Java: Persona cosmetics and emotes | Works |
| Modded Java skin → Bedrock players | Works |
| Bedrock player's skin → modded Java players | Works |
| Bedrock skin changes in-game (no rejoin) | Works |
| Java emote → Bedrock players | Partially works, see [Emotes](#emotes) |
| Bedrock emote → modded Java players | Partially works, see [Emotes](#emotes) |

Other things it does:

- Skins are deduplicated by SHA-256 content hash, so a skin worn by many players is stored once. Data is dropped when nobody online wears it.
- Skin changes are rate limited (5 seconds by default, like the mod's own server) and size checked (skin PNG up to 512 KiB, cape up to 256 KiB).
- Late joiners receive every active skin as soon as their client registers the mod's channels.
- Skins are sent to Bedrock players again when a player comes into range, because Pumpkin overwrites them with its own skin whenever it spawns a player for a Bedrock viewer.

## Requirements

- **Pumpkin**, built from commit `058f7ec` with the patches in `patches/` applied.
- **Rust** (from [rustup.rs](https://rustup.rs)) and **Git**, to build Pumpkin and the plugin.
- **Java players** need the [Bedrock Skins](https://github.com/BrandonItaly/Bedrock-Skins) mod (Fabric or NeoForge) to see and use custom models.
- **Bedrock players** need nothing extra.

Java players without the mod see normal Pumpkin skins. Custom geometry requires the mod on the client.

## Installation

### 1. Build a patched Pumpkin

The plugin is built against the patched plugin API, so keep the patched checkout next to the plugin folder (`pumpkin-patched` beside `bedrock-skins-bridge`).

```bash
git clone https://github.com/Pumpkin-MC/Pumpkin.git pumpkin-patched
cd pumpkin-patched
git checkout 058f7ec6fde8a8f24e3d6bb92344df48a2f07ffc
git apply ../bedrock-skins-bridge/patches/pumpkin-bedrock-all.patch
cargo build --release
```

On Windows PowerShell the commands are the same, with `..\bedrock-skins-bridge\patches\...` as the patch path.

The server binary is `target/release/pumpkin` (`pumpkin.exe` on Windows). Building takes a while and uses several GB of RAM. Use this binary to run your server. Worlds, config and the `plugins/` folder carry over unchanged.

If you already applied some of the patches, apply only the missing ones, in this order: `pumpkin-bedrock-player-list.patch`, `pumpkin-bedrock-skin-api.patch`, `pumpkin-bedrock-emote.patch`. `pumpkin-bedrock-all.patch` contains all three.

### 2. Build the plugin

Linux and macOS:

```bash
cd bedrock-skins-bridge
./build.sh
```

Windows (PowerShell):

```powershell
cd bedrock-skins-bridge
.\build.ps1
```

Both scripts expect the patched Pumpkin in `../pumpkin-patched`. To use another location, set `PUMPKIN_DIR` first (`export PUMPKIN_DIR=/path/to/pumpkin-patched`, or `$env:PUMPKIN_DIR = "C:\path\to\pumpkin-patched"`). They install the `wasm32-wasip2` Rust target if needed, run the tests, and build the plugin. If PowerShell refuses to run the script, run `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned` once.

A plain `cargo build` fails on purpose: it would use the unpatched upstream API.

### 3. Install the plugin

Copy `target/wasm32-wasip2/release/bedrock_skins_bridge.wasm` into your server's `plugins/` folder and start the server. When Pumpkin asks, approve `fs.read.data` and `fs.write.data` (used for the config file).

## Configuration

The config is created on first start at `plugins/data/bedrock-skins-bridge/config.toml`:

| Option | Default | Meaning |
|---|---|---|
| `bedrock_sync` | `true` | Send mod skins and emotes to Bedrock players. |
| `bedrock_capture` | `true` | Read Bedrock players' login skins, skin changes and emotes and pass them to Java players. This watches incoming packets, which costs a little CPU per packet. |
| `bedrock_resend_radius` | `48` | When a player comes within this many blocks of a Bedrock player, their skin is sent again. Keep it below your entity tracking range. |
| `skin_change_cooldown_ms` | `5000` | Minimum time between one player's skin changes. |
| `send_to_unregistered_clients` | `false` | Also send skin data to Java clients that never announced the mod's channels. |
| `debug` | `false` | Log what the bridge does (skin and emote traffic, parse failures). |

## Emotes

Java and Bedrock both identify an emote by its Persona piece UUID, and the mod bundles 15 Bedrock Character Creator emotes under those UUIDs. For example, `4c8ae710-df2e-47cd-814d-cc7bf21a3d67` is the overhead wave.

An emote only plays for a viewer who has it:

- A Bedrock client ignores an emote id it doesn't know. A Java emote therefore only plays for Bedrock players who own that emote.
- The Java mod can only play an id it has installed. Bedrock sends just the id, so the stock emotes in a Bedrock player's wheel do nothing on Java.
- To soften that, the plugin remembers the full animation of every emote a Java player plays. If a Bedrock player later plays the same id, Java viewers who lack it receive the animation too. This is kept in memory only and resets on restart.
- Emote ids that aren't UUIDs (custom emote packs) are only relayed to Java players.

Bedrock-to-Bedrock emotes are already relayed by Pumpkin itself.

## How it works

- **Java → server:** the plugin speaks the mod's protocol (`set_skin`, `skin_announce`, `request_skin_data`, `skin_update`, `set_cosmetics`, `play_emote` and so on) exactly as the mod's own server does.
- **Translation:** Bedrock skins are raw RGBA, the mod uses PNG. Mod skins are decoded and sent with a resource patch naming the geometry. Bedrock skins are re-encoded as PNG, and their geometry is passed to the mod, or replaced by a bundled humanoid geometry if the skin has none.
- **Bedrock → server:** Pumpkin hands plugins the raw bytes of Bedrock's `PlayerSkin` (93) and `Emote` (138) packets, which the plugin parses itself. A Bedrock client's login skin is read through a new plugin API function.
- **Server → Bedrock:** skins go out as a player-list entry plus a `PlayerSkin` packet, and emotes as an `Emote` packet. Pumpkin's plugin host couldn't send any of these before the patches.

### What the Pumpkin patches add

All patches only touch Pumpkin's plugin host, the plugin WIT definitions and one new packet struct:

| Patch | Adds |
|---|---|
| `pumpkin-bedrock-player-list.patch` | Sending the player-list packet from plugins. |
| `pumpkin-bedrock-skin-api.patch` | The `PlayerSkin` packet, and `get-login-skin` to read a Bedrock client's login skin. |
| `pumpkin-bedrock-emote.patch` | The clientbound `Emote` packet. |

They are small enough to propose upstream.

## Limitations

- **You must build and run a patched Pumpkin.** The patches are written for commit `058f7ec`. They may need updating for newer versions, and the plugin is built against that version's plugin API (`pumpkin:plugin@0.1.0`).
- **Java players need the mod** to see custom models.
- **Persona cosmetics don't cross over.** Java cosmetics can't be shown to Bedrock players, since Persona pieces need Marketplace content. Persona pieces from Bedrock players aren't shown to Java players either, only their skin image and geometry.
- **Animated textures aren't carried over.** The mod's animated textures reach Bedrock players as their base image, and animated skins from Bedrock aren't shown on Java.
- **Emotes need to be known by the viewer**, see [Emotes](#emotes).
- **Bedrock viewers get skins when players come into range.** Because Pumpkin overwrites skins when it spawns a player, expect a short delay (about a second) before a skin shows after approaching someone.
- **The skin sync itself has limits.** Skin PNGs are capped at 512 KiB and capes at 256 KiB. Skin changes are rate limited. Skins that fail validation or translation are skipped with a log message.
- **The first skin after joining can be dropped.** The mod sends its saved skin two client ticks after joining. If the plugin's channel registration arrives later, that first skin is lost until the player changes skin again.

## Credits

- [Bedrock Skins](https://github.com/BrandonItaly/Bedrock-Skins) by BrandonItaly, the Java mod this plugin serves.
- [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin), the Rust Minecraft server.

## License

GPL-3.0-only, see [LICENSE](LICENSE).
