//! MIDI I/O for communicating knob/button values with the Roto-Control.
//!
//! In MIDI mode, the device sends and receives CC messages.
//! We assign knobs to CC 1-8 and buttons to CC 9-16, all on channel 1.

use anyhow::{Context, Result, bail};
use log::{debug, warn};
use midir::{MidiInput, MidiOutput, MidiOutputConnection, MidiInputConnection};
use std::sync::mpsc;

pub const MIDI_CHANNEL: u8 = 0; // Channel 1 (0-indexed)
pub const KNOB_CC_BASE: u8 = 1; // Knobs use CC 1-8
pub const BUTTON_CC_BASE: u8 = 9; // Buttons use CC 9-16
pub const DISCORD_KNOB_CC_BASE: u8 = 17; // Discord knobs CC 17-24
pub const DISCORD_BUTTON_CC_BASE: u8 = 25; // Discord buttons CC 25-32
pub const NUM_CONTROLS: usize = 8;

/// Events received from the device.
#[derive(Debug)]
pub enum DeviceEvent {
    KnobTurn { index: usize, value: u8 },
    ButtonPress { index: usize, value: u8 },
    DiscordKnobTurn { index: usize, value: u8 },
    DiscordButtonPress { index: usize, value: u8 },
}

/// Open the MIDI output connection to the Roto-Control.
pub fn open_output() -> Result<MidiOutputConnection> {
    let midi_out = MidiOutput::new("rotocontrol-out")
        .context("Failed to create MIDI output")?;

    let ports = midi_out.ports();
    let port = ports
        .iter()
        .find(|p| {
            midi_out
                .port_name(p)
                .map(|n| n.contains("Roto-Control"))
                .unwrap_or(false)
        })
        .context("Roto-Control MIDI output port not found")?;

    let port_name = midi_out.port_name(port)?;
    debug!("Opening MIDI output: {}", port_name);

    midi_out
        .connect(port, "rotocontrol-out")
        .map_err(|e| anyhow::anyhow!("Failed to connect MIDI output: {}", e))
}

/// Open the MIDI input connection and return a receiver for device events.
pub fn open_input() -> Result<(MidiInputConnection<()>, mpsc::Receiver<DeviceEvent>)> {
    let midi_in = MidiInput::new("rotocontrol-in")
        .context("Failed to create MIDI input")?;

    let ports = midi_in.ports();
    let port = ports
        .iter()
        .find(|p| {
            midi_in
                .port_name(p)
                .map(|n| n.contains("Roto-Control"))
                .unwrap_or(false)
        })
        .context("Roto-Control MIDI input port not found")?;

    let port_name = midi_in.port_name(port)?;
    debug!("Opening MIDI input: {}", port_name);

    let (tx, rx) = mpsc::channel();

    let conn = midi_in
        .connect(
            port,
            "rotocontrol-in",
            move |_timestamp, message, _| {
                if message.len() != 3 {
                    return;
                }
                let status = message[0];
                let cc = message[1];
                let value = message[2];

                // We only care about CC messages on our channel
                if status != (0xB0 | MIDI_CHANNEL) {
                    return;
                }

                debug!("MIDI RX: CC {} = {} (ch {})", cc, value, status & 0x0F);

                if cc >= KNOB_CC_BASE && cc < KNOB_CC_BASE + NUM_CONTROLS as u8 {
                    let index = (cc - KNOB_CC_BASE) as usize;
                    let _ = tx.send(DeviceEvent::KnobTurn { index, value });
                } else if cc >= BUTTON_CC_BASE && cc < BUTTON_CC_BASE + NUM_CONTROLS as u8 {
                    let index = (cc - BUTTON_CC_BASE) as usize;
                    let _ = tx.send(DeviceEvent::ButtonPress { index, value });
                } else if cc >= DISCORD_KNOB_CC_BASE && cc < DISCORD_KNOB_CC_BASE + NUM_CONTROLS as u8 {
                    let index = (cc - DISCORD_KNOB_CC_BASE) as usize;
                    let _ = tx.send(DeviceEvent::DiscordKnobTurn { index, value });
                } else if cc >= DISCORD_BUTTON_CC_BASE && cc < DISCORD_BUTTON_CC_BASE + NUM_CONTROLS as u8 {
                    let index = (cc - DISCORD_BUTTON_CC_BASE) as usize;
                    let _ = tx.send(DeviceEvent::DiscordButtonPress { index, value });
                } else {
                    debug!("MIDI RX: ignoring CC {}", cc);
                }
            },
            (),
        )
        .map_err(|e| anyhow::anyhow!("Failed to connect MIDI input: {}", e))?;

    Ok((conn, rx))
}

/// Send a CC value to set a knob position (motor will move).
pub fn send_knob_value(conn: &mut MidiOutputConnection, index: usize, value: u8) -> Result<()> {
    if index >= NUM_CONTROLS {
        bail!("Knob index {} out of range", index);
    }
    let msg = [0xB0 | MIDI_CHANNEL, KNOB_CC_BASE + index as u8, value];
    conn.send(&msg)
        .map_err(|e| anyhow::anyhow!("Failed to send MIDI: {}", e))
}

/// Send a CC value to set a button LED state.
pub fn send_button_value(conn: &mut MidiOutputConnection, index: usize, value: u8) -> Result<()> {
    if index >= NUM_CONTROLS {
        bail!("Button index {} out of range", index);
    }
    let msg = [0xB0 | MIDI_CHANNEL, BUTTON_CC_BASE + index as u8, value];
    conn.send(&msg)
        .map_err(|e| anyhow::anyhow!("Failed to send MIDI: {}", e))
}

/// Send a CC value to set a Discord knob position.
pub fn send_discord_knob_value(conn: &mut MidiOutputConnection, index: usize, value: u8) -> Result<()> {
    if index >= NUM_CONTROLS {
        bail!("Discord knob index {} out of range", index);
    }
    let msg = [0xB0 | MIDI_CHANNEL, DISCORD_KNOB_CC_BASE + index as u8, value];
    conn.send(&msg)
        .map_err(|e| anyhow::anyhow!("Failed to send MIDI: {}", e))
}

/// Send a CC value to set a Discord button LED state.
pub fn send_discord_button_value(conn: &mut MidiOutputConnection, index: usize, value: u8) -> Result<()> {
    if index >= NUM_CONTROLS {
        bail!("Discord button index {} out of range", index);
    }
    let msg = [0xB0 | MIDI_CHANNEL, DISCORD_BUTTON_CC_BASE + index as u8, value];
    conn.send(&msg)
        .map_err(|e| anyhow::anyhow!("Failed to send MIDI: {}", e))
}
