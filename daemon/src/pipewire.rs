//! PipeWire stream enumeration and volume control via pw-dump / wpctl.

use crate::config::Config;
use anyhow::{Context, Result, bail};
use log::debug;
use serde::Deserialize;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct AudioStream {
    pub id: u32,
    pub app_name: String,
    pub media_name: Option<String>,
    pub color_scheme: Option<u8>,
    pub volume: f64,
    pub muted: bool,
}

#[derive(Deserialize)]
struct PwNode {
    id: u32,
    info: Option<PwNodeInfo>,
}

#[derive(Deserialize)]
struct PwNodeInfo {
    props: Option<PwNodeProps>,
}

#[derive(Deserialize)]
struct PwNodeProps {
    #[serde(rename = "media.class")]
    media_class: Option<String>,
    #[serde(rename = "application.name")]
    application_name: Option<String>,
    #[serde(rename = "node.name")]
    node_name: Option<String>,
    #[serde(rename = "application.process.binary")]
    process_binary: Option<String>,
    #[serde(rename = "pipewire.access.portal.app_id")]
    portal_app_id: Option<String>,
    #[serde(rename = "media.name")]
    media_name: Option<String>,
}

/// List all audio output streams currently connected in PipeWire.
pub fn list_streams(config: &Config) -> Result<Vec<AudioStream>> {
    let output = Command::new("pw-dump")
        .output()
        .context("Failed to run pw-dump. Is PipeWire running?")?;

    if !output.status.success() {
        bail!("pw-dump failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let nodes: Vec<PwNode> = serde_json::from_slice(&output.stdout)
        .context("Failed to parse pw-dump output")?;

    let mut streams = Vec::new();

    for node in nodes {
        let info = match &node.info {
            Some(i) => i,
            None => continue,
        };
        let props = match &info.props {
            Some(p) => p,
            None => continue,
        };
        let media_class = match &props.media_class {
            Some(c) => c,
            None => continue,
        };

        if media_class != "Stream/Output/Audio" {
            continue;
        }

        let default_name = props.application_name
            .clone()
            .or_else(|| props.node_name.clone())
            .unwrap_or_else(|| format!("Stream {}", node.id));

        let binary = props.process_binary.as_deref().unwrap_or("");
        let app_id = props.portal_app_id.as_deref().unwrap_or("");
        let resolved = config.resolve(binary, app_id, &default_name);

        // Truncate to 12 chars (Roto-Control display limit is 13 with null)
        let app_name = truncate_to_chars(&resolved.name, 12);

        // Get media_name: prefer MPRIS title if configured, otherwise PipeWire media.name
        let media_name = resolved.mpris_player.as_deref()
            .and_then(crate::config::query_mpris_title)
            .or_else(|| {
                props.media_name.as_deref()
                    .filter(|m| !is_generic_media_name(m, &app_name))
                    .map(String::from)
            })
            .map(|m| truncate_to_chars(&m, 12));

        let (volume, muted) = get_volume_and_mute(node.id)?;

        streams.push(AudioStream {
            id: node.id,
            app_name,
            media_name,
            color_scheme: resolved.color,
            volume,
            muted,
        });
    }

    debug!("Found {} audio streams", streams.len());
    Ok(streams)
}

/// Truncate a string to at most `max` characters (not bytes).
fn truncate_to_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Returns true if a media.name looks generic/internal rather than useful content.
fn is_generic_media_name(media_name: &str, app_name: &str) -> bool {
    let lower = media_name.to_lowercase();
    lower.contains("playback")
        || lower.contains("playstream")
        || lower.contains("dummy")
        || lower == app_name.to_lowercase()
        || media_name.trim().is_empty()
}

/// Get volume (0.0-1.0+) and mute state for a PipeWire node.
fn get_volume_and_mute(id: u32) -> Result<(f64, bool)> {
    let output = Command::new("wpctl")
        .args(["get-volume", &id.to_string()])
        .output()
        .context("Failed to run wpctl")?;

    let text = String::from_utf8_lossy(&output.stdout);
    // Output format: "Volume: 0.45" or "Volume: 0.45 [MUTED]"
    let muted = text.contains("[MUTED]");
    let volume = text
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.0);

    Ok((volume, muted))
}

/// Set volume for a PipeWire node. Value is 0.0 to 1.0.
pub fn set_volume(id: u32, volume: f64) -> Result<()> {
    let vol = volume.clamp(0.0, 1.0);
    Command::new("wpctl")
        .args(["set-volume", &id.to_string(), &format!("{:.2}", vol)])
        .output()
        .context("Failed to set volume")?;
    Ok(())
}

/// Toggle mute for a PipeWire node.
pub fn toggle_mute(id: u32) -> Result<()> {
    Command::new("wpctl")
        .args(["set-mute", &id.to_string(), "toggle"])
        .output()
        .context("Failed to toggle mute")?;
    Ok(())
}

/// Get the current mute state for a PipeWire node.
pub fn is_muted(id: u32) -> Result<bool> {
    let (_, muted) = get_volume_and_mute(id)?;
    Ok(muted)
}
