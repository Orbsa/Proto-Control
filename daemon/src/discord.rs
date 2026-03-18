//! Discord RPC client for per-user voice channel volume/mute control.
//!
//! Connects to Discord's local IPC socket, authenticates via OAuth,
//! and exposes voice channel members for the main loop to control.

use anyhow::{bail, Context, Result};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

// IPC opcodes
const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;

static NONCE: AtomicU64 = AtomicU64::new(1);
fn next_nonce() -> String {
    NONCE.fetch_add(1, Ordering::Relaxed).to_string()
}

#[derive(Debug, Clone)]
pub struct VoiceMember {
    pub user_id: String,
    pub nick: String,
    pub volume: u16,  // 0-200 Discord IPC scale
    pub muted: bool,
}

pub enum Command {
    SetVolume { user_id: String, volume: u16 },
    SetMute { user_id: String, muted: bool },
}

pub struct DiscordHandle {
    pub members_rx: mpsc::Receiver<Vec<VoiceMember>>,
    pub cmd_tx: mpsc::Sender<Command>,
}

/// Start the Discord RPC client in a background thread.
pub fn start(client_id: String, client_secret: String) -> DiscordHandle {
    let (members_tx, members_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();

    thread::Builder::new()
        .name("discord".into())
        .spawn(move || {
            loop {
                match run_client(&client_id, &client_secret, &members_tx, &cmd_rx) {
                    Ok(()) => break,
                    Err(e) => {
                        warn!("Discord RPC error: {}. Reconnecting in 5s...", e);
                        // Send empty members to clear page 2
                        let _ = members_tx.send(vec![]);
                        std::thread::sleep(Duration::from_secs(5));
                    }
                }
            }
        })
        .expect("Failed to spawn Discord thread");

    DiscordHandle { members_rx, cmd_tx }
}

fn run_client(
    client_id: &str,
    client_secret: &str,
    members_tx: &mpsc::Sender<Vec<VoiceMember>>,
    cmd_rx: &mpsc::Receiver<Command>,
) -> Result<()> {
    // 1. Connect to IPC socket
    let socket_path = find_discord_socket()
        .context("Discord IPC socket not found. Is Discord running?")?;
    info!("Connecting to Discord IPC at {}", socket_path.display());

    let mut stream = UnixStream::connect(&socket_path)
        .context("Failed to connect to Discord IPC")?;

    // 2. Handshake
    let handshake = json!({"v": 1, "client_id": client_id});
    write_frame(&mut stream, OP_HANDSHAKE, &handshake)?;
    let (_, ready) = read_frame(&mut stream)?;

    let self_user_id = ready["data"]["user"]["id"]
        .as_str().unwrap_or("").to_string();
    info!("Discord connected as user {}", self_user_id);

    // 3. Authenticate
    authenticate(&mut stream, client_id, client_secret)?;
    info!("Discord authenticated");

    // 4. Subscribe to global voice channel select event
    send_rpc(&mut stream, "SUBSCRIBE", json!({}), Some("VOICE_CHANNEL_SELECT"))?;

    // 5. Get initial voice channel state
    send_rpc(&mut stream, "GET_SELECTED_VOICE_CHANNEL", json!({}), None)?;

    // 6. Event loop
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut current_members: Vec<VoiceMember> = vec![];
    let mut current_channel_id: Option<String> = None;

    loop {
        // Process commands from main thread
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::SetVolume { user_id, volume } => {
                    send_rpc(&mut stream, "SET_USER_VOICE_SETTINGS", json!({
                        "user_id": user_id,
                        "volume": volume,
                    }), None)?;
                }
                Command::SetMute { user_id, muted } => {
                    send_rpc(&mut stream, "SET_USER_VOICE_SETTINGS", json!({
                        "user_id": user_id,
                        "mute": muted,
                    }), None)?;
                }
            }
        }

        // Read IPC messages
        let (opcode, msg) = match read_frame(&mut stream) {
            Ok(frame) => frame,
            Err(e) if is_timeout(&e) => continue,
            Err(e) => return Err(e),
        };

        if opcode != OP_FRAME {
            continue;
        }

        let cmd = msg["cmd"].as_str().unwrap_or("");
        let evt = msg["evt"].as_str().unwrap_or("");

        match (cmd, evt) {
            ("DISPATCH", "VOICE_CHANNEL_SELECT") => {
                info!("Voice channel select raw data: {}", msg["data"]);
                let new_channel = msg["data"]["channel_id"].as_str().map(String::from);
                info!("Voice channel select: {:?} -> {:?}", current_channel_id, new_channel);
                if new_channel != current_channel_id {
                    handle_channel_switch(
                        &mut stream, &mut current_channel_id, new_channel,
                        &mut current_members, members_tx,
                    )?;
                }
            }

            // Response to GET_SELECTED_VOICE_CHANNEL
            ("GET_SELECTED_VOICE_CHANNEL", _) if evt != "ERROR" => {
                let new_channel = msg["data"]["id"].as_str().map(String::from);
                if new_channel.is_some() {
                    current_members = parse_voice_states(
                        &msg["data"]["voice_states"], &self_user_id,
                    );
                    if let Some(ref id) = new_channel {
                        subscribe_channel_events(&mut stream, id)?;
                    }
                    current_channel_id = new_channel;
                    let _ = members_tx.send(current_members.clone());
                }
            }

            ("DISPATCH", "VOICE_STATE_CREATE") => {
                let uid = msg["data"]["user"]["id"].as_str().unwrap_or("");
                if uid != self_user_id {
                    if let Some(m) = parse_voice_member(&msg["data"]) {
                        if !current_members.iter().any(|cm| cm.user_id == m.user_id) {
                            current_members.push(m);
                            let _ = members_tx.send(current_members.clone());
                        }
                    }
                }
            }

            ("DISPATCH", "VOICE_STATE_DELETE") => {
                let uid = msg["data"]["user"]["id"].as_str().unwrap_or("");
                let before = current_members.len();
                current_members.retain(|m| m.user_id != uid);
                if current_members.len() != before {
                    let _ = members_tx.send(current_members.clone());
                }
            }

            ("DISPATCH", "VOICE_STATE_UPDATE") => {
                let uid = msg["data"]["user"]["id"].as_str().unwrap_or("");
                if uid != self_user_id {
                    if let Some(member) = current_members.iter_mut().find(|m| m.user_id == uid) {
                        let mut changed = false;
                        if let Some(v) = msg["data"]["volume"].as_f64() {
                            let new_vol = v.round() as u16;
                            if member.volume != new_vol {
                                member.volume = new_vol;
                                changed = true;
                            }
                        }
                        if let Some(m) = msg["data"]["mute"].as_bool() {
                            if member.muted != m {
                                member.muted = m;
                                changed = true;
                            }
                        }
                        if changed {
                            let _ = members_tx.send(current_members.clone());
                        }
                    }
                }
            }

            (_, "ERROR") => {
                let code = msg["data"]["code"].as_i64().unwrap_or(0);
                let message = msg["data"]["message"].as_str().unwrap_or("unknown");
                warn!("Discord RPC error ({}): {} — {}", cmd, code, message);
            }

            _ => {
                debug!("Discord: cmd={}, evt={}", cmd, evt);
            }
        }
    }
}

fn handle_channel_switch(
    stream: &mut UnixStream,
    current_channel_id: &mut Option<String>,
    new_channel: Option<String>,
    current_members: &mut Vec<VoiceMember>,
    members_tx: &mpsc::Sender<Vec<VoiceMember>>,
) -> Result<()> {
    // Unsubscribe from old channel
    if let Some(old_id) = current_channel_id.as_deref() {
        unsubscribe_channel_events(stream, old_id)?;
    }

    *current_channel_id = new_channel;

    if current_channel_id.is_some() {
        // Fetch fresh member list
        send_rpc(stream, "GET_SELECTED_VOICE_CHANNEL", json!({}), None)?;
        // Response will be handled in the main event loop
    } else {
        // Left voice channel
        current_members.clear();
        let _ = members_tx.send(vec![]);
    }

    Ok(())
}

// ---- IPC Protocol ----

fn write_frame(stream: &mut UnixStream, opcode: u32, payload: &Value) -> Result<()> {
    let json_bytes = serde_json::to_vec(payload)?;
    let mut buf = Vec::with_capacity(8 + json_bytes.len());
    buf.extend_from_slice(&opcode.to_le_bytes());
    buf.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&json_bytes);
    stream.write_all(&buf)?;
    stream.flush()?;
    debug!("Discord TX: op={} {}", opcode, String::from_utf8_lossy(&json_bytes));
    Ok(())
}

fn read_frame(stream: &mut UnixStream) -> Result<(u32, Value)> {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header)?;
    let opcode = u32::from_le_bytes(header[0..4].try_into().unwrap());
    let length = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
    let mut payload = vec![0u8; length];
    stream.read_exact(&mut payload)?;
    let json: Value = serde_json::from_slice(&payload)?;
    debug!("Discord RX: op={} {}", opcode, String::from_utf8_lossy(&payload));
    Ok((opcode, json))
}

fn send_rpc(stream: &mut UnixStream, cmd: &str, args: Value, evt: Option<&str>) -> Result<()> {
    let mut msg = json!({
        "cmd": cmd,
        "args": args,
        "nonce": next_nonce(),
    });
    if let Some(e) = evt {
        msg["evt"] = json!(e);
    }
    write_frame(stream, OP_FRAME, &msg)
}

fn is_timeout(e: &anyhow::Error) -> bool {
    if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
        return io_err.kind() == std::io::ErrorKind::WouldBlock
            || io_err.kind() == std::io::ErrorKind::TimedOut;
    }
    // serde_json wraps IO errors
    if let Some(serde_err) = e.downcast_ref::<serde_json::Error>() {
        if serde_err.is_io() {
            return true; // read_exact timeout wrapped in serde
        }
    }
    false
}

// ---- Authentication ----

#[derive(Serialize, Deserialize)]
struct TokenData {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

fn authenticate(stream: &mut UnixStream, client_id: &str, client_secret: &str) -> Result<()> {
    // Try saved token first
    if let Some(token) = load_token() {
        if try_authenticate(stream, &token.access_token)? {
            return Ok(());
        }
        // Try refresh
        if let Some(ref refresh) = token.refresh_token {
            if let Some(new_token) = refresh_token(client_id, client_secret, refresh) {
                if try_authenticate(stream, &new_token.access_token)? {
                    save_token(&new_token);
                    return Ok(());
                }
            }
        }
    }

    // Full authorize flow
    info!("Please authorize Rotool in Discord...");
    let code = authorize(stream, client_id)?;
    let token = exchange_code(client_id, client_secret, &code)?;
    if !try_authenticate(stream, &token.access_token)? {
        bail!("AUTHENTICATE failed after fresh authorization");
    }
    save_token(&token);
    Ok(())
}

fn try_authenticate(stream: &mut UnixStream, access_token: &str) -> Result<bool> {
    send_rpc(stream, "AUTHENTICATE", json!({"access_token": access_token}), None)?;
    // Need to read with full timeout for auth response
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (_, resp) = read_frame(stream)?;
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;

    if resp["evt"].as_str() == Some("ERROR") {
        debug!("AUTHENTICATE failed: {}", resp["data"]["message"]);
        return Ok(false);
    }
    Ok(true)
}

fn authorize(stream: &mut UnixStream, client_id: &str) -> Result<String> {
    send_rpc(stream, "AUTHORIZE", json!({
        "client_id": client_id,
        "scopes": ["rpc", "rpc.voice.read", "rpc.voice.write"],
    }), None)?;

    // Wait for user to click authorize (could take a while)
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    let (_, resp) = read_frame(stream)?;
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;

    if resp["evt"].as_str() == Some("ERROR") {
        bail!("AUTHORIZE failed: {}", resp["data"]["message"]);
    }

    resp["data"]["code"].as_str()
        .map(String::from)
        .context("No code in AUTHORIZE response")
}

fn exchange_code(client_id: &str, client_secret: &str, code: &str) -> Result<TokenData> {
    let body = format!(
        "client_id={}&client_secret={}&grant_type=authorization_code&code={}&redirect_uri=http://localhost",
        client_id, client_secret, code,
    );
    token_request(&body)
}

fn refresh_token(client_id: &str, client_secret: &str, refresh: &str) -> Option<TokenData> {
    let body = format!(
        "client_id={}&client_secret={}&grant_type=refresh_token&refresh_token={}",
        client_id, client_secret, refresh,
    );
    token_request(&body).ok()
}

fn token_request(body: &str) -> Result<TokenData> {
    let output = std::process::Command::new("curl")
        .args([
            "-s", "-X", "POST",
            "https://discord.com/api/oauth2/token",
            "-H", "Content-Type: application/x-www-form-urlencoded",
            "-d", body,
        ])
        .output()
        .context("Failed to run curl for Discord token exchange")?;

    if !output.status.success() {
        bail!("curl failed: {}", String::from_utf8_lossy(&output.stderr));
    }

    let json: Value = serde_json::from_slice(&output.stdout)
        .context("Failed to parse token response")?;

    if let Some(err) = json["error"].as_str() {
        bail!("Token exchange error: {}", err);
    }

    Ok(TokenData {
        access_token: json["access_token"].as_str()
            .context("No access_token in response")?.to_string(),
        refresh_token: json["refresh_token"].as_str().map(String::from),
    })
}

fn token_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rotocontrol").join("discord_token.json"))
}

fn load_token() -> Option<TokenData> {
    let path = token_path()?;
    let contents = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&contents).ok()
}

fn save_token(token: &TokenData) {
    if let Some(path) = token_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(token) {
            if let Err(e) = std::fs::write(&path, json) {
                warn!("Failed to save Discord token: {}", e);
            } else {
                debug!("Saved Discord token to {}", path.display());
            }
        }
    }
}

// ---- Voice channel events ----

const CHANNEL_EVENTS: &[&str] = &[
    "VOICE_STATE_UPDATE",
    "VOICE_STATE_CREATE",
    "VOICE_STATE_DELETE",
];

fn subscribe_channel_events(stream: &mut UnixStream, channel_id: &str) -> Result<()> {
    for event in CHANNEL_EVENTS {
        send_rpc(stream, "SUBSCRIBE", json!({"channel_id": channel_id}), Some(event))?;
    }
    Ok(())
}

fn unsubscribe_channel_events(stream: &mut UnixStream, channel_id: &str) -> Result<()> {
    for event in CHANNEL_EVENTS {
        send_rpc(stream, "UNSUBSCRIBE", json!({"channel_id": channel_id}), Some(event))?;
    }
    Ok(())
}

// ---- Voice state parsing ----

fn parse_voice_states(voice_states: &Value, self_user_id: &str) -> Vec<VoiceMember> {
    let Some(states) = voice_states.as_array() else {
        return vec![];
    };
    states.iter()
        .filter_map(|s| parse_voice_member(s))
        .filter(|m| m.user_id != self_user_id)
        .collect()
}

fn parse_voice_member(state: &Value) -> Option<VoiceMember> {
    let user_id = state["user"]["id"].as_str()?.to_string();
    let nick = state["nick"].as_str()
        .or_else(|| state["user"]["username"].as_str())
        .unwrap_or("?")
        .to_string();
    let volume = state["volume"].as_f64().unwrap_or(100.0).round() as u16;
    let muted = state["mute"].as_bool().unwrap_or(false);

    Some(VoiceMember { user_id, nick, volume, muted })
}

// ---- IPC socket discovery ----

fn find_discord_socket() -> Option<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", unsafe { libc::getuid() }));

    let prefixes = [
        PathBuf::from(&runtime_dir),
        PathBuf::from(&runtime_dir).join("app/com.discordapp.Discord"),
        PathBuf::from("/tmp"),
    ];

    for prefix in &prefixes {
        for i in 0..10 {
            let path = prefix.join(format!("discord-ipc-{}", i));
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}
