//! MIDI I/O for communicating knob/button values with the Roto-Control.
//!
//! All setups share MIDI channel 1 (0-indexed: 0) with unique CC ranges:
//!
//!   PipeWire:  knobs CC  1-16, buttons CC 17-32
//!   Discord:   knobs CC 33-48, buttons CC 49-64
//!   TeamSpeak: knobs CC 65-80, buttons CC 81-96
//!
//! 2 pages × 8 controls = 16 per setup, 96 CCs total.

use anyhow::{Context, Result, bail};
use log::debug;
use midir::{MidiInput, MidiOutput, MidiOutputConnection, MidiInputConnection};
use std::sync::mpsc;

pub const MIDI_CHANNEL: u8 = 0; // Channel 1 (0-indexed)

pub const KNOB_CC_BASE: u8           = 1;   // PW knobs   CC  1-16
pub const BUTTON_CC_BASE: u8         = 17;  // PW buttons CC 17-32
pub const DISCORD_KNOB_CC_BASE: u8   = 33;  // Discord knobs   CC 33-48
pub const DISCORD_BUTTON_CC_BASE: u8 = 49;  // Discord buttons CC 49-64
pub const TS3_KNOB_CC_BASE: u8       = 65;  // TS3 knobs   CC 65-80
pub const TS3_BUTTON_CC_BASE: u8     = 81;  // TS3 buttons CC 81-96

pub const NUM_CONTROLS: usize = 16; // 2 pages × 8 controls per setup

/// Events received from the device.
#[derive(Debug)]
pub enum DeviceEvent {
    KnobTurn          { index: usize, value: u8 },
    ButtonPress       { index: usize, value: u8 },
    DiscordKnobTurn   { index: usize, value: u8 },
    DiscordButtonPress{ index: usize, value: u8 },
    Ts3KnobTurn       { index: usize, value: u8 },
    Ts3ButtonPress    { index: usize, value: u8 },
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
                let cc     = message[1];
                let value  = message[2];

                if status != (0xB0 | MIDI_CHANNEL) {
                    return;
                }

                debug!("MIDI RX: CC {} = {}", cc, value);

                let n = NUM_CONTROLS as u8;
                let evt = if cc >= KNOB_CC_BASE && cc < KNOB_CC_BASE + n {
                    DeviceEvent::KnobTurn { index: (cc - KNOB_CC_BASE) as usize, value }
                } else if cc >= BUTTON_CC_BASE && cc < BUTTON_CC_BASE + n {
                    DeviceEvent::ButtonPress { index: (cc - BUTTON_CC_BASE) as usize, value }
                } else if cc >= DISCORD_KNOB_CC_BASE && cc < DISCORD_KNOB_CC_BASE + n {
                    DeviceEvent::DiscordKnobTurn { index: (cc - DISCORD_KNOB_CC_BASE) as usize, value }
                } else if cc >= DISCORD_BUTTON_CC_BASE && cc < DISCORD_BUTTON_CC_BASE + n {
                    DeviceEvent::DiscordButtonPress { index: (cc - DISCORD_BUTTON_CC_BASE) as usize, value }
                } else if cc >= TS3_KNOB_CC_BASE && cc < TS3_KNOB_CC_BASE + n {
                    DeviceEvent::Ts3KnobTurn { index: (cc - TS3_KNOB_CC_BASE) as usize, value }
                } else if cc >= TS3_BUTTON_CC_BASE && cc < TS3_BUTTON_CC_BASE + n {
                    DeviceEvent::Ts3ButtonPress { index: (cc - TS3_BUTTON_CC_BASE) as usize, value }
                } else {
                    debug!("MIDI RX: ignoring CC {}", cc);
                    return;
                };
                let _ = tx.send(evt);
            },
            (),
        )
        .map_err(|e| anyhow::anyhow!("Failed to connect MIDI input: {}", e))?;

    Ok((conn, rx))
}

pub fn send_knob_value(conn: &mut MidiOutputConnection, index: usize, value: u8) -> Result<()> {
    if index >= NUM_CONTROLS { bail!("Knob index {} out of range", index); }
    conn.send(&[0xB0 | MIDI_CHANNEL, KNOB_CC_BASE + index as u8, value])
        .map_err(|e| anyhow::anyhow!("Failed to send MIDI: {}", e))
}

pub fn send_button_value(conn: &mut MidiOutputConnection, index: usize, value: u8) -> Result<()> {
    if index >= NUM_CONTROLS { bail!("Button index {} out of range", index); }
    conn.send(&[0xB0 | MIDI_CHANNEL, BUTTON_CC_BASE + index as u8, value])
        .map_err(|e| anyhow::anyhow!("Failed to send MIDI: {}", e))
}

pub fn send_discord_knob_value(conn: &mut MidiOutputConnection, index: usize, value: u8) -> Result<()> {
    if index >= NUM_CONTROLS { bail!("Discord knob index {} out of range", index); }
    conn.send(&[0xB0 | MIDI_CHANNEL, DISCORD_KNOB_CC_BASE + index as u8, value])
        .map_err(|e| anyhow::anyhow!("Failed to send MIDI: {}", e))
}

pub fn send_discord_button_value(conn: &mut MidiOutputConnection, index: usize, value: u8) -> Result<()> {
    if index >= NUM_CONTROLS { bail!("Discord button index {} out of range", index); }
    conn.send(&[0xB0 | MIDI_CHANNEL, DISCORD_BUTTON_CC_BASE + index as u8, value])
        .map_err(|e| anyhow::anyhow!("Failed to send MIDI: {}", e))
}

pub fn send_ts3_knob_value(conn: &mut MidiOutputConnection, index: usize, value: u8) -> Result<()> {
    if index >= NUM_CONTROLS { bail!("TS3 knob index {} out of range", index); }
    conn.send(&[0xB0 | MIDI_CHANNEL, TS3_KNOB_CC_BASE + index as u8, value])
        .map_err(|e| anyhow::anyhow!("Failed to send MIDI: {}", e))
}

pub fn send_ts3_button_value(conn: &mut MidiOutputConnection, index: usize, value: u8) -> Result<()> {
    if index >= NUM_CONTROLS { bail!("TS3 button index {} out of range", index); }
    conn.send(&[0xB0 | MIDI_CHANNEL, TS3_BUTTON_CC_BASE + index as u8, value])
        .map_err(|e| anyhow::anyhow!("Failed to send MIDI: {}", e))
}
