//! TeamSpeak 3 IPC client for per-user voice channel volume/mute control.
//!
//! Connects to the rotocontrol_ts3.so plugin via Unix domain socket.
//! The plugin acts as socket server; the daemon reconnects automatically.
//!
//! IPC protocol (newline-delimited JSON):
//!   Plugin → daemon: {"type":"members","members":[{"id":N,"nick":"...","muted":bool,"self_muted":bool,"self_deafened":bool},...]}
//!   Daemon → plugin: {"type":"set_volume","client_id":N,"volume":V}  (V: 0-200, 100 = 0 dB)
//!   Daemon → plugin: {"type":"set_mute","client_id":N,"muted":bool}
//!
//! Volume mapping: dB = (volume - 100) * 0.4  →  range -40 dB … +40 dB

use anyhow::{Context, Result};
use log::{debug, info, warn};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

pub fn default_socket_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    // Flatpak TS3 maps its $HOME to ~/.var/app/com.teamspeak.TeamSpeak3/ on the host,
    // so the plugin writes the socket at that path.
    let flatpak = format!("{}/.var/app/com.teamspeak.TeamSpeak3/.ts3client/rotocontrol-ts3.sock", home);
    let native  = format!("{}/.ts3client/rotocontrol-ts3.sock", home);
    if std::path::Path::new(&flatpak).parent().map_or(false, |p| p.exists()) {
        flatpak
    } else {
        native
    }
}

#[derive(Debug, Clone)]
pub struct TsMember {
    pub client_id: u16,
    pub nick: String,
    pub muted: bool,
    pub volume: u16, // 0-200, tracked locally; default 100 = 0 dB (no change)
    pub self_muted: bool,    // user muted their own mic
    pub self_deafened: bool, // user deafened themselves
}

impl TsMember {
    /// Sort key: 0 = active, 1 = self-muted only, 2 = deafened.
    pub fn activity_key(&self) -> u8 {
        if self.self_deafened { 2 }
        else if self.self_muted { 1 }
        else { 0 }
    }
}

pub enum Command {
    SetVolume { client_id: u16, volume: u16 },
    SetMute { client_id: u16, muted: bool },
}

pub struct TsHandle {
    pub members_rx: mpsc::Receiver<Vec<TsMember>>,
    pub cmd_tx: mpsc::Sender<Command>,
}

/// Start the TeamSpeak IPC client in a background thread.
pub fn start(socket_path: String) -> TsHandle {
    let (members_tx, members_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();

    thread::Builder::new()
        .name("teamspeak".into())
        .spawn(move || loop {
            match run_client(&socket_path, &members_tx, &cmd_rx) {
                Ok(()) => break,
                Err(e) => {
                    warn!("TeamSpeak IPC error: {}. Reconnecting in 5s...", e);
                    let _ = members_tx.send(vec![]);
                    thread::sleep(Duration::from_secs(5));
                }
            }
        })
        .expect("Failed to spawn teamspeak thread");

    TsHandle { members_rx, cmd_tx }
}

// ---- wire types ----

#[derive(Deserialize)]
struct RawMember {
    id: u16,
    nick: String,
    muted: bool,
    #[serde(default)]
    self_muted: bool,
    #[serde(default)]
    self_deafened: bool,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(rename = "type")]
    msg_type: String,
    members: Option<Vec<RawMember>>,
}

// ---- client loop ----

fn run_client(
    socket_path: &str,
    members_tx: &mpsc::Sender<Vec<TsMember>>,
    cmd_rx: &mpsc::Receiver<Command>,
) -> Result<()> {
    let stream = UnixStream::connect(socket_path)
        .with_context(|| format!("Failed to connect to TS3 plugin at {}", socket_path))?;
    info!("Connected to TeamSpeak plugin socket");

    // 50 ms read timeout so we can interleave command sends
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;

    let mut write_half = stream.try_clone().context("Failed to clone TS3 socket")?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    loop {
        // Drain outbound commands
        while let Ok(cmd) = cmd_rx.try_recv() {
            let json = match cmd {
                Command::SetVolume { client_id, volume } => format!(
                    "{{\"type\":\"set_volume\",\"client_id\":{},\"volume\":{}}}\n",
                    client_id, volume
                ),
                Command::SetMute { client_id, muted } => format!(
                    "{{\"type\":\"set_mute\",\"client_id\":{},\"muted\":{}}}\n",
                    client_id, muted
                ),
            };
            debug!("TS3 TX: {}", line.trim_end());
            write_half.write_all(json.as_bytes())?;
        }

        // Try to read one line (appends to `line`; partial reads are preserved)
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(()), // EOF — plugin disconnected
            Ok(_) => {
                // Only process when we have a complete line (ends with \n)
                if line.ends_with('\n') {
                    let trimmed = line.trim();
                    debug!("TS3 RX: {}", trimmed);
                    match serde_json::from_str::<RawMessage>(trimmed) {
                        Ok(msg) if msg.msg_type == "members" => {
                            let members: Vec<TsMember> = msg
                                .members
                                .unwrap_or_default()
                                .into_iter()
                                .map(|m| TsMember {
                                    client_id: m.id,
                                    nick: m.nick,
                                    muted: m.muted,
                                    volume: 100,
                                    self_muted: m.self_muted,
                                    self_deafened: m.self_deafened,
                                })
                                .collect();
                            info!("TeamSpeak: {} members in channel", members.len());
                            let _ = members_tx.send(members);
                        }
                        Ok(_) => {}
                        Err(e) => warn!("Failed to parse TS3 message '{}': {}", trimmed, e),
                    }
                    line.clear();
                }
            }
            Err(e) if is_timeout(&e) => {} // no data yet, loop back
            Err(e) => return Err(e.into()),
        }
    }
}

fn is_timeout(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut
}
