//! Configuration loading and stream name resolution.

use log::{debug, warn};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(default)]
    pub streams: Vec<StreamOverride>,
    #[serde(default = "default_true")]
    pub pipewire_enabled: bool,
    pub discord: Option<DiscordConfig>,
    pub teamspeak: Option<TeamSpeakConfig>,
    #[serde(default)]
    pub discord_users: Vec<UserOverride>,
    #[serde(default)]
    pub teamspeak_users: Vec<UserOverride>,
}

fn default_true() -> bool { true }

impl Default for Config {
    fn default() -> Self {
        Self {
            streams: vec![],
            pipewire_enabled: true,
            discord: None,
            teamspeak: None,
            discord_users: vec![],
            teamspeak_users: vec![],
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DiscordConfig {
    pub client_id: String,
    pub client_secret: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TeamSpeakConfig {
    /// Path to the Unix socket created by the TS3 plugin.
    /// Auto-detected: checks Flatpak path first, then native ~/.ts3client/.
    #[serde(default = "default_ts3_socket")]
    pub socket_path: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_ts3_socket() -> String {
    crate::teamspeak::default_socket_path()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StreamOverride {
    pub binary: Option<String>,
    pub app_id: Option<String>,
    pub name: String,
    /// MPRIS player name for `playerctl` to query current track title.
    pub mpris_player: Option<String>,
    /// Top/knob color scheme index (0-84).
    pub color: Option<u8>,
    /// Bottom/button accent color (only visible when media_name is present).
    pub accent_color: Option<u8>,
    /// If true, this stream is hidden from the device and daemon.
    #[serde(default)]
    pub ignored: bool,
}

/// Per-user settings for Discord or TeamSpeak.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserOverride {
    /// Discord username or TeamSpeak nickname.
    pub name: String,
    /// Color scheme index (0-84). None = hash-based "random" color.
    pub color: Option<u8>,
    /// Sort priority: lower numbers appear first on the device.
    pub priority: Option<i32>,
}

struct BuiltinDefault {
    binary: &'static str,
    name: &'static str,
    mpris_player: Option<&'static str>,
    color: Option<u8>,
}

const BUILTIN_DEFAULTS: &[BuiltinDefault] = &[
    // Discord on Linux ships as a wrapped Electron app with this binary name.
    BuiltinDefault { binary: ".Discord-wrapped", name: "Discord", mpris_player: None, color: Some(62) },
];

/// Result of resolving a stream's config.
pub struct ResolvedStream {
    pub name: String,
    pub mpris_player: Option<String>,
    pub color: Option<u8>,
    pub accent_color: Option<u8>,
    pub ignored: bool,
}

impl Config {
    /// Load config from `~/.config/rotocontrol/config.toml`.
    /// Returns default (empty) config if the file is missing or unreadable.
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            debug!("Could not determine config directory");
            return Self::default();
        };

        match std::fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(config) => {
                    debug!("Loaded config from {}", path.display());
                    config
                }
                Err(e) => {
                    warn!("Failed to parse {}: {}", path.display(), e);
                    Self::default()
                }
            },
            Err(_) => {
                debug!("No config file at {}", path.display());
                Self::default()
            }
        }
    }

    /// Save config to `~/.config/rotocontrol/config.toml`.
    pub fn save(&self) -> Result<(), String> {
        let path = config_path().ok_or_else(|| "Cannot determine config path".to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let toml = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, toml).map_err(|e| e.to_string())?;
        debug!("Saved config to {}", path.display());
        Ok(())
    }

    /// Resolve stream display name and MPRIS player from binary/app_id.
    /// Priority: config file entries > built-in defaults > fallback.
    pub fn resolve(&self, binary: &str, app_id: &str, default: &str) -> ResolvedStream {
        // Check user config first
        for entry in &self.streams {
            let matches = entry.binary.as_deref().is_some_and(|b| b == binary)
                || entry.app_id.as_deref().is_some_and(|id| id == app_id);
            if matches {
                return ResolvedStream {
                    name: entry.name.clone(),
                    mpris_player: entry.mpris_player.clone(),
                    color: entry.color,
                    accent_color: entry.accent_color,
                    ignored: entry.ignored,
                };
            }
        }

        // Check built-in defaults
        for d in BUILTIN_DEFAULTS {
            if d.binary == binary {
                return ResolvedStream {
                    name: d.name.to_string(),
                    mpris_player: d.mpris_player.map(String::from),
                    color: d.color,
                    accent_color: None,
                    ignored: false,
                };
            }
        }

        ResolvedStream {
            name: default.to_string(),
            mpris_player: None,
            color: None,
            accent_color: None,
            ignored: false,
        }
    }

    /// Look up a saved user override by name (case-insensitive prefix match).
    pub fn discord_user(&self, name: &str) -> Option<&UserOverride> {
        let lower = name.to_lowercase();
        self.discord_users.iter().find(|u| u.name.to_lowercase() == lower)
    }

    /// Look up a saved TS3 user override by name (case-insensitive prefix match).
    pub fn ts3_user(&self, name: &str) -> Option<&UserOverride> {
        let lower = name.to_lowercase();
        self.teamspeak_users.iter().find(|u| u.name.to_lowercase() == lower)
    }
}

/// Query MPRIS for the current track title via `playerctl`.
pub fn query_mpris_title(player: &str) -> Option<String> {
    let output = Command::new("playerctl")
        .args(["-p", player, "metadata", "title"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let title = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if title.is_empty() { None } else { Some(title) }
}

pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("proto-control").join("config.toml"))
}

/// Path for runtime state files (e.g. active member lists written by the daemon).
pub fn state_path(filename: &str) -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("proto-control").join(filename))
}
