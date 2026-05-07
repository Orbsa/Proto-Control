//! MPV integration via JSON IPC over Unix domain socket.
//!
//! MPV exposes a JSON-based IPC protocol when started with the
//! protocontrol.lua script (which sets input-ipc-server), or manually:
//!   mpv --input-ipc-server=/tmp/proto-control-mpv.sock
//!
//! Architecture matches the TeamSpeak integration: a background thread
//! connects to the socket, receives state updates, and sends commands
//! via mpsc channels.
//!
//! IPC protocol (newline-delimited JSON):
//!   Daemon → MPV: {"command": ["set_property", "speed", 1.5]}
//!   Daemon → MPV: {"command": ["set_property", "pause", true]}
//!   MPV → Daemon: {"error": "success", "data": ...}
//!   MPV → Daemon: {"event": "property-change", "name": "speed", "data": 1.5}

use anyhow::{Context, Result};
use log::{debug, info};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const DEFAULT_SOCKET: &str = "/tmp/proto-control-mpv.sock";

/// Commands sent from the main loop to the MPV background thread.
pub enum Command {
    SetSpeed(f64),
    SetPause(bool),
    FrameStep,
    FrameBackStep,
}

/// State updates sent from the MPV background thread to the main loop.
#[derive(Debug, Clone)]
pub struct MpvState {
    pub speed: f64,
    pub paused: bool,
    pub fps: f64,
    pub connected: bool,
}

/// Number of detents on the frame jog wheel (matches 24fps film standard).
pub const FRAME_STEPS: u8 = 24;

pub struct MpvHandle {
    pub state_rx: mpsc::Receiver<MpvState>,
    pub cmd_tx: mpsc::Sender<Command>,
}

/// Start the MPV IPC client in a background thread.
pub fn start(socket_path: String) -> MpvHandle {
    let (state_tx, state_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();

    thread::Builder::new()
        .name("mpv".into())
        .spawn(move || loop {
            match run_client(&socket_path, &state_tx, &cmd_rx) {
                Ok(()) => break,
                Err(e) => {
                    debug!("MPV IPC: {}. Retrying in 5s...", e);
                    let _ = state_tx.send(MpvState {
                        speed: 1.0,
                        paused: false,
                        fps: 24.0,
                        connected: false,
                    });
                    thread::sleep(Duration::from_secs(5));
                }
            }
        })
        .expect("Failed to spawn mpv thread");

    MpvHandle { state_rx, cmd_tx }
}

pub fn default_socket_path() -> String {
    DEFAULT_SOCKET.to_string()
}

// ---- client loop ----

fn run_client(
    socket_path: &str,
    state_tx: &mpsc::Sender<MpvState>,
    cmd_rx: &mpsc::Receiver<Command>,
) -> Result<()> {
    let stream = UnixStream::connect(socket_path)
        .with_context(|| format!("Failed to connect to MPV at {}", socket_path))?;
    info!("Connected to MPV socket");

    // 50ms read timeout so we can interleave command sends
    stream.set_read_timeout(Some(Duration::from_millis(50)))?;

    let mut write_half = stream.try_clone().context("Failed to clone MPV socket")?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    // Observe speed, pause, and fps properties so MPV pushes changes to us
    send_json(
        &mut write_half,
        r#"{"command": ["observe_property", 1, "speed"]}"#,
    )?;
    send_json(
        &mut write_half,
        r#"{"command": ["observe_property", 2, "pause"]}"#,
    )?;
    send_json(
        &mut write_half,
        r#"{"command": ["observe_property", 3, "estimated-vf-fps"]}"#,
    )?;

    // Query initial state
    send_json(
        &mut write_half,
        r#"{"command": ["get_property", "speed"]}"#,
    )?;
    send_json(
        &mut write_half,
        r#"{"command": ["get_property", "pause"]}"#,
    )?;
    send_json(
        &mut write_half,
        r#"{"command": ["get_property", "estimated-vf-fps"]}"#,
    )?;

    let mut current_speed: f64 = 1.0;
    let mut current_paused: bool = false;
    let mut current_fps: f64 = 24.0;

    // Signal connected
    let _ = state_tx.send(MpvState {
        speed: current_speed,
        paused: current_paused,
        fps: current_fps,
        connected: true,
    });

    loop {
        // Drain outbound commands
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                Command::SetSpeed(speed) => {
                    let msg = format!(
                        "{{\"command\": [\"set_property\", \"speed\", {}]}}",
                        speed
                    );
                    send_json(&mut write_half, &msg)?;
                }
                Command::SetPause(paused) => {
                    let msg = format!(
                        "{{\"command\": [\"set_property\", \"pause\", {}]}}",
                        paused
                    );
                    send_json(&mut write_half, &msg)?;
                }
                Command::FrameStep => {
                    send_json(&mut write_half, r#"{"command": ["frame-step"]}"#)?;
                }
                Command::FrameBackStep => {
                    send_json(&mut write_half, r#"{"command": ["frame-back-step"]}"#)?;
                }
            }
        }

        // Try to read one line
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(()), // EOF — MPV quit
            Ok(_) => {
                if line.ends_with('\n') {
                    let trimmed = line.trim();
                    debug!("MPV RX: {}", trimmed);

                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                        let mut changed = false;

                        // Property-change events from observe_property
                        if val.get("event").and_then(|v| v.as_str()) == Some("property-change") {
                            match val.get("name").and_then(|v| v.as_str()) {
                                Some("speed") => {
                                    if let Some(s) = val.get("data").and_then(|v| v.as_f64()) {
                                        current_speed = s;
                                        changed = true;
                                    }
                                }
                                Some("pause") => {
                                    if let Some(p) = val.get("data").and_then(|v| v.as_bool()) {
                                        current_paused = p;
                                        changed = true;
                                    }
                                }
                                Some("estimated-vf-fps") => {
                                    if let Some(f) = val.get("data").and_then(|v| v.as_f64()) {
                                        if f > 0.0 {
                                            current_fps = f;
                                            changed = true;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }

                        // Command responses (get_property replies)
                        if val.get("error").and_then(|v| v.as_str()) == Some("success") {
                            if let Some(data) = val.get("data") {
                                if let Some(s) = data.as_f64() {
                                    // Could be speed or fps — fps is typically > 10
                                    if s > 0.0 && s <= 10.0 {
                                        current_speed = s;
                                        changed = true;
                                    } else if s > 10.0 {
                                        current_fps = s;
                                        changed = true;
                                    }
                                }
                                if let Some(p) = data.as_bool() {
                                    current_paused = p;
                                    changed = true;
                                }
                            }
                        }

                        if changed {
                            let _ = state_tx.send(MpvState {
                                speed: current_speed,
                                paused: current_paused,
                                fps: current_fps,
                                connected: true,
                            });
                        }
                    }
                    line.clear();
                }
            }
            Err(e) if is_timeout(&e) => {} // no data yet
            Err(e) => return Err(e.into()),
        }
    }
}

fn send_json(stream: &mut UnixStream, msg: &str) -> Result<()> {
    debug!("MPV TX: {}", msg);
    stream.write_all(msg.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn is_timeout(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut
}

// ---- Speed mapping (spring-return) ----
//
// The speed knob uses CentreIndent haptic mode: the motor provides a physical
// magnetic detent at the midpoint of [0, MAX_CC].  By setting MAX_CC to 90
// the midpoint (CC 45) lands at approximately -90° (12 o'clock).
//
// CC mapping:
//   0   = 0.0x (fully CCW, pause)
//   45  = 1.0x (centre indent, 12 o'clock)
//   90  = 2.0x (fully CW, double speed)

/// Maximum CC value for the speed knob range.
pub const MAX_CC: u8 = 90;

/// CC value corresponding to the centre / home / 1.0x position (midpoint of range).
pub const HOME_CC: u8 = MAX_CC / 2; // 45

/// How long after the last knob movement before we consider the user to have
/// released the knob and snap speed back to 1.0x.  The centre-indent motor
/// pull-back takes ~150-200ms, so we wait a bit longer to avoid fighting it.
pub const SPRING_RETURN_MS: u64 = 300;

/// Map a CC value (0–MAX_CC) to a playback speed (0.0–2.0x).
/// Centre (HOME_CC=45) = 1.0x, linear on each side.
pub fn cc_to_speed(cc: u8) -> f64 {
    let cc = cc.min(MAX_CC);
    if cc <= HOME_CC {
        // CCW: 0→0.0x, 45→1.0x
        cc as f64 / HOME_CC as f64
    } else {
        // CW: 45→1.0x, 90→2.0x
        1.0 + (cc as f64 - HOME_CC as f64) / (MAX_CC as f64 - HOME_CC as f64)
    }
}

/// Map a playback speed (0.0–2.0x) back to a CC value (0–MAX_CC).
pub fn speed_to_cc(speed: f64) -> u8 {
    let speed = speed.clamp(0.0, 2.0);
    if speed <= 1.0 {
        (speed * HOME_CC as f64).round() as u8
    } else {
        (HOME_CC as f64 + (speed - 1.0) * (MAX_CC as f64 - HOME_CC as f64)).round() as u8
    }
}

/// Format a speed value for the button display label.
pub fn speed_label(speed: f64) -> String {
    if speed < 0.01 {
        "Paused".to_string()
    } else {
        format!("{:.2}x", speed)
    }
}

