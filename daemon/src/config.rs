//! Configuration loading and stream name resolution.

use log::{debug, warn};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub streams: Vec<StreamOverride>,
}

#[derive(Debug, Deserialize)]
pub struct StreamOverride {
    pub binary: Option<String>,
    pub app_id: Option<String>,
    pub name: String,
    /// MPRIS player name for `playerctl` to query current track title.
    pub mpris_player: Option<String>,
    /// Color scheme index (0-84) for the device display.
    pub color: Option<u8>,
}

struct BuiltinDefault {
    binary: &'static str,
    name: &'static str,
    mpris_player: Option<&'static str>,
    color: Option<u8>,
}

const BUILTIN_DEFAULTS: &[BuiltinDefault] = &[
    BuiltinDefault { binary: "zen", name: "Zen", mpris_player: None, color: None },
    BuiltinDefault { binary: ".Discord-wrapped", name: "Discord", mpris_player: None, color: Some(62) }, // #2F52A2 blue
    BuiltinDefault { binary: "tidal-hifi", name: "Tidal", mpris_player: Some("tidal-hifi"), color: None },
];

/// Result of resolving a stream's config: display name + optional MPRIS player + color.
pub struct ResolvedStream {
    pub name: String,
    pub mpris_player: Option<String>,
    pub color: Option<u8>,
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
                };
            }
        }

        ResolvedStream {
            name: default.to_string(),
            mpris_player: None,
            color: None,
        }
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

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rotocontrol").join("config.toml"))
}
