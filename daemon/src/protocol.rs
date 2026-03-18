//! Binary serial protocol for communicating with the Roto-Control device.
//!
//! Commands:  0x5A <group> <subcommand> <len_hi> <len_lo> [data...]
//! Responses: 0xA5 <status> [data...]
//!
//! Status 0x00 = OK, anything else is an error code.

use anyhow::{bail, Context, Result};
use log::debug;
use std::io::{Read, Write};
use std::time::Duration;

const MSG_COMMAND: u8 = 0x5A;
const MSG_RESPONSE: u8 = 0xA5;
const MSG_RESPONSE_OK: u8 = 0x00;
const MSG_RESPONSE_UNCONFIGURED: u8 = 0xFD;

// Command groups
const GENERAL: u8 = 0x01;
const MIDI: u8 = 0x02;
#[allow(dead_code)]
const PLUGIN: u8 = 0x03;
const MAINTENANCE: u8 = 0x04;

// General subcommands
const GENERAL_GET_FW_VERSION: u8 = 0x01;
const GENERAL_GET_ATARI_MODE: u8 = 0x02;
const GENERAL_SET_ATARI_MODE: u8 = 0x03;
const GENERAL_START_CONFIG_UPDATE: u8 = 0x04;
const GENERAL_END_CONFIG_UPDATE: u8 = 0x05;

// MIDI subcommands
const MIDI_SET_SETUP_NAME: u8 = 0x04;
const MIDI_SET_KNOB_CONTROL_CONFIG: u8 = 0x07;
const MIDI_SET_SWITCH_CONTROL_CONFIG: u8 = 0x08;
const MIDI_CLEAR_CONTROL_CONFIG: u8 = 0x09;

const NAME_LENGTH: usize = 13;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Midi,
    Plugin,
    Mix,
}

impl Mode {
    fn from_index(i: u8) -> Result<Self> {
        match i {
            0 => Ok(Mode::Midi),
            1 => Ok(Mode::Plugin),
            2 => Ok(Mode::Mix),
            _ => bail!("Unknown mode index: {}", i),
        }
    }

    fn to_index(self) -> u8 {
        match self {
            Mode::Midi => 0,
            Mode::Plugin => 1,
            Mode::Mix => 2,
        }
    }
}

#[derive(Debug)]
pub struct FirmwareVersion {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
    pub commit: String,
}

#[derive(Debug)]
pub struct ModeInfo {
    pub mode: Mode,
    pub first_control_index: u8,
}

#[derive(Debug, Clone, Copy)]
pub enum ControlMode {
    Cc7Bit = 0,
    Cc14Bit = 1,
    Nrpn7Bit = 2,
    Nrpn14Bit = 3,
}

#[derive(Debug, Clone, Copy)]
pub enum KnobHapticMode {
    Normal = 0,
    NStep = 1,
    CentreIndent = 2,
}

#[derive(Debug, Clone, Copy)]
pub enum SwitchHapticMode {
    Push = 0,
    Toggle = 1,
}

#[derive(Debug)]
pub struct MidiKnobConfig {
    pub setup_index: u8,
    pub control_index: u8,
    pub control_mode: ControlMode,
    pub control_channel: u8,
    pub control_param: u8,
    pub nrpn_address: u16,
    pub min_value: u16,
    pub max_value: u16,
    pub control_name: String,
    pub color_scheme: u8,
    pub haptic_mode: KnobHapticMode,
    pub haptic_indent1: u8,
    pub haptic_indent2: u8,
    pub haptic_steps: u8,
    pub step_names: Vec<String>,
}

#[derive(Debug)]
pub struct MidiButtonConfig {
    pub setup_index: u8,
    pub control_index: u8,
    pub control_mode: ControlMode,
    pub control_channel: u8,
    pub control_param: u8,
    pub nrpn_address: u16,
    pub min_value: u16,
    pub max_value: u16,
    pub control_name: String,
    pub color_scheme: u8,
    pub led_on_color: u8,
    pub led_off_color: u8,
    pub haptic_mode: SwitchHapticMode,
    pub haptic_steps: u8,
    pub step_names: Vec<String>,
}

pub struct Device {
    port: Box<dyn serialport::SerialPort>,
}

impl Device {
    pub fn new(port: Box<dyn serialport::SerialPort>) -> Self {
        Self { port }
    }

    /// Send a command and read the response.
    /// Returns the response data bytes (after the status byte).
    fn send_command(&mut self, group: u8, subcommand: u8, data: &[u8]) -> Result<Vec<u8>> {
        let len = data.len() as u16;
        let mut packet = vec![
            MSG_COMMAND,
            group,
            subcommand,
            (len >> 8) as u8,
            (len & 0xFF) as u8,
        ];
        packet.extend_from_slice(data);

        debug!(
            "TX: {}",
            packet.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ")
        );
        self.port.write_all(&packet)?;
        self.port.flush()?;

        // Read response header byte, skipping any unsolicited device notifications (0x5A).
        // The device can send notifications (e.g. setup changes) at any time.
        let mut header = [0u8; 1];
        loop {
            self.port.read_exact(&mut header).context("Reading response header")?;
            if header[0] == MSG_RESPONSE {
                break;
            }
            if header[0] == MSG_COMMAND {
                // Unsolicited device notification — drain it and keep looking
                debug!("Skipping unsolicited device message (0x5A)");
                let original_timeout = self.port.timeout();
                self.port.set_timeout(Duration::from_millis(50))?;
                let mut drain = [0u8; 256];
                loop {
                    match self.port.read(&mut drain) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => continue,
                    }
                }
                self.port.set_timeout(original_timeout)?;
                continue;
            }
            bail!("Expected response 0xA5, got 0x{:02x}", header[0]);
        }

        // Read status byte
        let mut status = [0u8; 1];
        self.port.read_exact(&mut status).context("Reading response status")?;

        if status[0] != MSG_RESPONSE_OK && status[0] != MSG_RESPONSE_UNCONFIGURED {
            bail!("Device returned error status: 0x{:02x}", status[0]);
        }

        // Read remaining response data with a short drain timeout.
        // Most commands return no data after the status byte.
        let original_timeout = self.port.timeout();
        self.port.set_timeout(Duration::from_millis(100))?;
        let mut response_data = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            match self.port.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => response_data.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
                Err(e) => {
                    self.port.set_timeout(original_timeout)?;
                    return Err(e.into());
                }
            }
        }
        self.port.set_timeout(original_timeout)?;

        debug!(
            "RX: a5 00 {}",
            response_data.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ")
        );

        Ok(response_data)
    }

    /// Send a command that wraps in START_CONFIG_UPDATE / END_CONFIG_UPDATE.
    fn send_config_update(&mut self, group: u8, subcommand: u8, data: &[u8]) -> Result<()> {
        self.start_config_update()?;
        self.send_command(group, subcommand, data)?;
        self.end_config_update()?;
        Ok(())
    }

    /// Begin a config update transaction. Batch multiple configs before calling end_config_update.
    pub fn start_config_update(&mut self) -> Result<()> {
        self.send_command(GENERAL, GENERAL_START_CONFIG_UPDATE, &[])?;
        Ok(())
    }

    /// End a config update transaction.
    pub fn end_config_update(&mut self) -> Result<()> {
        self.send_command(GENERAL, GENERAL_END_CONFIG_UPDATE, &[])?;
        Ok(())
    }

    pub fn get_version(&mut self) -> Result<FirmwareVersion> {
        let data = self.send_command(GENERAL, GENERAL_GET_FW_VERSION, &[])?;
        if data.len() < 10 {
            bail!("Version response too short: {} bytes", data.len());
        }
        Ok(FirmwareVersion {
            major: data[0],
            minor: data[1],
            patch: data[2],
            commit: String::from_utf8_lossy(&data[3..10])
                .trim_end_matches('\0')
                .to_string(),
        })
    }

    pub fn get_mode(&mut self) -> Result<ModeInfo> {
        let data = self.send_command(GENERAL, GENERAL_GET_ATARI_MODE, &[])?;
        if data.len() < 2 {
            bail!("Mode response too short: {} bytes", data.len());
        }
        Ok(ModeInfo {
            mode: Mode::from_index(data[0])?,
            first_control_index: data[1],
        })
    }

    pub fn set_mode(&mut self, mode: Mode, first_control_index: u8) -> Result<()> {
        self.send_command(GENERAL, GENERAL_SET_ATARI_MODE, &[mode.to_index(), first_control_index])?;
        Ok(())
    }

    /// Set the name of a MIDI setup page (wraps in config update transaction).
    pub fn set_setup_name(&mut self, setup_index: u8, name: &str) -> Result<()> {
        let mut payload = vec![setup_index];
        payload.extend_from_slice(&to_padded_string(name, NAME_LENGTH));
        self.send_config_update(MIDI, MIDI_SET_SETUP_NAME, &payload)?;
        Ok(())
    }

    /// Clear all knob and button configs (wraps in config update transaction).
    pub fn clear_all(&mut self) -> Result<()> {
        self.start_config_update()?;
        for i in 0..8u8 {
            self.send_clear_knob(0, i)?;
            self.send_clear_button(0, i)?;
        }
        self.end_config_update()?;
        Ok(())
    }

    /// Send knob config (raw, no transaction wrapper — use inside a batch).
    pub fn send_midi_knob_config(&mut self, config: &MidiKnobConfig) -> Result<()> {
        let mut payload = Vec::new();
        payload.push(config.setup_index);
        payload.push(config.control_index);
        payload.push(config.control_mode as u8);
        payload.push(config.control_channel);
        payload.push(config.control_param);
        payload.push((config.nrpn_address >> 8) as u8);
        payload.push((config.nrpn_address & 0xFF) as u8);
        payload.push((config.min_value >> 8) as u8);
        payload.push((config.min_value & 0xFF) as u8);
        payload.push((config.max_value >> 8) as u8);
        payload.push((config.max_value & 0xFF) as u8);
        payload.extend_from_slice(&to_padded_string(&config.control_name, NAME_LENGTH));
        payload.push(config.color_scheme);
        payload.push(config.haptic_mode as u8);
        payload.push(config.haptic_indent1);
        payload.push(config.haptic_indent2);
        payload.push(config.haptic_steps);
        for name in &config.step_names {
            payload.extend_from_slice(&to_padded_string(name, NAME_LENGTH));
        }
        self.send_command(MIDI, MIDI_SET_KNOB_CONTROL_CONFIG, &payload)?;
        Ok(())
    }

    /// Send button config (raw, no transaction wrapper — use inside a batch).
    pub fn send_midi_button_config(&mut self, config: &MidiButtonConfig) -> Result<()> {
        let mut payload = Vec::new();
        payload.push(config.setup_index);
        payload.push(config.control_index);
        payload.push(config.control_mode as u8);
        payload.push(config.control_channel);
        payload.push(config.control_param);
        payload.push((config.nrpn_address >> 8) as u8);
        payload.push((config.nrpn_address & 0xFF) as u8);
        payload.push((config.min_value >> 8) as u8);
        payload.push((config.min_value & 0xFF) as u8);
        payload.push((config.max_value >> 8) as u8);
        payload.push((config.max_value & 0xFF) as u8);
        payload.extend_from_slice(&to_padded_string(&config.control_name, NAME_LENGTH));
        payload.push(config.color_scheme);
        payload.push(config.led_on_color);
        payload.push(config.led_off_color);
        payload.push(config.haptic_mode as u8);
        payload.push(config.haptic_steps);
        for name in &config.step_names {
            payload.extend_from_slice(&to_padded_string(name, NAME_LENGTH));
        }
        self.send_command(MIDI, MIDI_SET_SWITCH_CONTROL_CONFIG, &payload)?;
        Ok(())
    }

    /// Clear a knob config (raw, no transaction wrapper).
    pub fn send_clear_knob(&mut self, setup_index: u8, control_index: u8) -> Result<()> {
        let data = [setup_index, 0, control_index];
        self.send_command(MIDI, MIDI_CLEAR_CONTROL_CONFIG, &data)?;
        Ok(())
    }

    /// Clear a button config (raw, no transaction wrapper).
    pub fn send_clear_button(&mut self, setup_index: u8, control_index: u8) -> Result<()> {
        let data = [setup_index, 1, control_index];
        self.send_command(MIDI, MIDI_CLEAR_CONTROL_CONFIG, &data)?;
        Ok(())
    }
}

/// Pad/truncate a string to exactly `len` bytes, null-terminated.
fn to_padded_string(s: &str, len: usize) -> Vec<u8> {
    let mut bytes = s.as_bytes().to_vec();
    bytes.truncate(len - 1); // leave room for null terminator
    bytes.resize(len, 0);
    bytes
}
