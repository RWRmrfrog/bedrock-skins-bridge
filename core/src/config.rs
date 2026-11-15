//! `plugins/data/bedrock-skins-bridge/config.toml`: flat `key = value` lines.

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Show mod skins to Bedrock players (needs the Pumpkin patches in `patches/`).
    pub bedrock_sync: bool,
    /// Read Bedrock players' login skins, in-game skin changes and emotes. Registers a packet hook that sees
    /// every incoming play packet, so it costs a little CPU per packet.
    pub bedrock_capture: bool,
    /// Re-send a skin to a Bedrock player when the owner comes within this many blocks.
    pub bedrock_resend_radius: f64,
    /// Minimum time between one player's skin changes (the mod's server uses 5 s).
    pub skin_change_cooldown_ms: u64,
    /// Send mod payloads to Java clients that never registered the mod's channels. Vanilla
    /// clients ignore unknown payloads, so this only costs bandwidth; it helps loaders that
    /// do not announce channels in the play phase.
    pub send_to_unregistered_clients: bool,
    pub debug: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bedrock_sync: true,
            bedrock_capture: true,
            bedrock_resend_radius: 48.0,
            skin_change_cooldown_ms: 5000,
            send_to_unregistered_clients: false,
            debug: false,
        }
    }
}

pub const DEFAULT_FILE: &str = r#"# Bedrock Skins Bridge
#
# Show Java players' Bedrock Skins mod skins to Bedrock players.
# Needs a Pumpkin build with both patches in patches/ applied;
# without them Pumpkin drops the packets and nothing happens.
bedrock_sync = true

# Read Bedrock players' login skins, in-game skin changes and emotes and pass them to modded
# Java players (the login skin needs patches/pumpkin-bedrock-skin-api.patch).
# This watches incoming packets, which costs a little CPU per packet.
bedrock_capture = true

# When a player comes within this many blocks of a Bedrock player, their skin is sent again
# (Pumpkin's own spawn packets overwrite it). Keep it below your entity tracking range.
bedrock_resend_radius = 48

# Minimum milliseconds between one player's skin changes (the mod's own server uses 5000).
skin_change_cooldown_ms = 5000

# Also send skin data to Java clients that never announced the mod's channels.
send_to_unregistered_clients = false

debug = false
"#;

impl Config {
    /// Parses the file; unknown keys and bad values are reported, and defaults kept for them.
    #[must_use]
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut c = Self::default();
        let mut problems = Vec::new();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() || line.starts_with('[') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                problems.push(format!("line {}: expected key = value", n + 1));
                continue;
            };
            let (k, v) = (k.trim(), v.trim().trim_matches('"'));
            let ok = match k {
                "bedrock_sync" => v.parse().map(|b| c.bedrock_sync = b).is_ok(),
                "bedrock_capture" => v.parse().map(|b| c.bedrock_capture = b).is_ok(),
                "bedrock_resend_radius" => v
                    .parse::<f64>()
                    .ok()
                    .filter(|r| *r > 0.0)
                    .map(|r| c.bedrock_resend_radius = r)
                    .is_some(),
                "skin_change_cooldown_ms" => v.parse().map(|m| c.skin_change_cooldown_ms = m).is_ok(),
                "send_to_unregistered_clients" => v.parse().map(|b| c.send_to_unregistered_clients = b).is_ok(),
                "debug" => v.parse().map(|b| c.debug = b).is_ok(),
                _ => {
                    problems.push(format!("line {}: unknown setting {k}", n + 1));
                    continue;
                }
            };
            if !ok {
                problems.push(format!("line {}: bad value for {k}: {v}", n + 1));
            }
        }
        (c, problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_file_parses_to_defaults() {
        let (c, problems) = Config::parse(DEFAULT_FILE);
        assert!(problems.is_empty(), "{problems:?}");
        assert_eq!(c, Config::default());
    }

    #[test]
    fn values_and_problems() {
        let (c, problems) =
            Config::parse("bedrock_sync = false\nbedrock_resend_radius = 80 # far\nnope = 1\ndebug = maybe\n");
        assert!(!c.bedrock_sync);
        assert!((c.bedrock_resend_radius - 80.0).abs() < f64::EPSILON);
        assert_eq!(problems.len(), 2);
    }
}
