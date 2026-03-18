mod config;
mod midi;
mod pipewire;
mod protocol;
mod tray;

use anyhow::{Context, Result};
use log::{debug, info, warn};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_KNOBS: usize = 8;
const STREAM_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Color scheme indices chosen to be visually distinct across the 85-entry palette.
/// Avoids black/dark entries so text remains readable.
const COLOR_POOL: &[u8] = &[
    0,  // #FF94A6 pink
    1,  // #FFA529 orange
    3,  // #F7F47C yellow
    5,  // #1AFF2F green
    7,  // #5CFFE8 teal
    9,  // #5480E4 blue
    11, // #D86CE4 purple
    14, // #FF3636 red
];

fn main() -> Result<()> {
    env_logger::init();

    // Set up Ctrl+C / SIGTERM handler
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let shutdown = shutdown.clone();
        ctrlc::set_handler(move || {
            shutdown.store(true, Ordering::SeqCst);
        })?;
    }

    // 1. Connect to the Roto-Control serial port
    let port_path = find_roto_control_port()
        .context("Could not find Roto-Control device. Is it plugged in?")?;
    info!("Found Roto-Control at {}", port_path);

    let port = serialport::new(&port_path, 115_200)
        .timeout(Duration::from_secs(2))
        .open()
        .with_context(|| format!("Failed to open {}", port_path))?;

    let mut dev = protocol::Device::new(port);

    // Verify firmware
    let version = dev.get_version()?;
    info!("Firmware: {}.{}.{} ({})", version.major, version.minor, version.patch, version.commit);

    // 2. Switch to MIDI mode and set setup name
    info!("Switching to MIDI mode...");
    dev.set_mode(protocol::Mode::Midi, 0)?;
    dev.set_setup_name(0, "Rotool")?;

    // 3. Load config and enumerate PipeWire streams
    let config = config::Config::load();
    let streams = pipewire::list_streams(&config)?;
    let num_assigned = streams.len().min(MAX_KNOBS);
    info!("Found {} audio streams, assigning {} to knobs", streams.len(), num_assigned);

    if num_assigned == 0 {
        warn!("No audio streams found. Play some audio and restart.");
        return Ok(());
    }

    // 4. Configure all knobs and buttons in a single transaction
    let mut assigned_streams: Vec<pipewire::AudioStream> = streams.into_iter().take(MAX_KNOBS).collect();
    apply_stream_config(&mut dev, &assigned_streams)?;

    // 5. Open MIDI connections
    info!("Opening MIDI connections...");
    let mut midi_out = midi::open_output()?;
    let (_midi_in_conn, midi_rx) = midi::open_input()?;

    // 6. Set initial knob positions to match current volumes
    // Small delay to let MIDI connection stabilize
    std::thread::sleep(Duration::from_millis(200));
    sync_midi_state(&mut midi_out, &assigned_streams)?;

    // 7. Start tray icon (quit triggers shutdown flag)
    let tray_shutdown = shutdown.clone();
    let _tray_handle = tray::spawn(tray_shutdown);

    info!("Ready! Turn knobs to adjust volume, press buttons to mute/unmute.");

    // 8. Main event loop
    let mut last_scan = Instant::now();
    while !shutdown.load(Ordering::SeqCst) {
        match midi_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(midi::DeviceEvent::KnobTurn { index, value }) => {
                if index < assigned_streams.len() {
                    let stream = &mut assigned_streams[index];
                    let new_volume = cc_to_volume(value);
                    debug!("Knob {}: {} -> {:.0}%", index, stream.app_name, new_volume * 100.0);
                    if let Err(e) = pipewire::set_volume(stream.id, new_volume) {
                        warn!("Failed to set volume for {}: {}", stream.app_name, e);
                    }
                    stream.volume = new_volume;
                }
            }
            Ok(midi::DeviceEvent::ButtonPress { index, value }) => {
                if index < assigned_streams.len() {
                    let stream = &mut assigned_streams[index];
                    // Toggle mode: device alternates 127/0 on each press
                    let now_muted = value > 0;
                    debug!("Button {}: {} muted={}", index, stream.app_name, now_muted);

                    // Set mute state to match button
                    if now_muted != stream.muted {
                        if let Err(e) = pipewire::toggle_mute(stream.id) {
                            warn!("Failed to toggle mute for {}: {}", stream.app_name, e);
                        }
                        stream.muted = now_muted;
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                warn!("MIDI input disconnected");
                break;
            }
        }

        // Periodically rescan PipeWire streams
        if last_scan.elapsed() >= STREAM_POLL_INTERVAL {
            last_scan = Instant::now();
            if let Err(e) = rescan_streams(
                &mut dev,
                &mut assigned_streams,
                &mut midi_out,
                &config,
            ) {
                warn!("Stream rescan failed: {}", e);
            }
        }
    }

    // 9. Cleanup: clear the device display
    info!("Shutting down, clearing display...");
    if let Err(e) = dev.clear_all() {
        warn!("Failed to clear display: {}", e);
    }

    Ok(())
}

/// Configure all stream knobs/buttons and clear unused slots in one transaction.
fn apply_stream_config(dev: &mut protocol::Device, streams: &[pipewire::AudioStream]) -> Result<()> {
    dev.start_config_update()?;
    for (i, stream) in streams.iter().enumerate() {
        info!("Knob {}: {} (id={}, vol={:.0}%{})",
            i + 1, stream.app_name, stream.id,
            stream.volume * 100.0,
            if stream.muted { " MUTED" } else { "" }
        );
        dev.send_midi_knob_config(&make_knob_config(i, stream))?;
        dev.send_midi_button_config(&make_button_config(i, stream))?;
    }
    for i in streams.len()..MAX_KNOBS {
        dev.send_clear_knob(0, i as u8)?;
        dev.send_clear_button(0, i as u8)?;
    }
    dev.end_config_update()?;
    Ok(())
}

/// Send MIDI CC to set knob positions and button LEDs to match stream state.
fn sync_midi_state(midi_out: &mut midir::MidiOutputConnection, streams: &[pipewire::AudioStream]) -> Result<()> {
    for (i, stream) in streams.iter().enumerate() {
        let cc_value = volume_to_cc(stream.volume);
        midi::send_knob_value(midi_out, i, cc_value)?;
        midi::send_button_value(midi_out, i, if stream.muted { 127 } else { 0 })?;
    }
    Ok(())
}

/// Rescan PipeWire streams and update device config for any changes.
fn rescan_streams(
    dev: &mut protocol::Device,
    assigned: &mut Vec<pipewire::AudioStream>,
    midi_out: &mut midir::MidiOutputConnection,
    config: &config::Config,
) -> Result<()> {
    let fresh = pipewire::list_streams(config)?;
    let fresh: Vec<_> = fresh.into_iter().take(MAX_KNOBS).collect();

    let old_ids: Vec<u32> = assigned.iter().map(|s| s.id).collect();
    let new_ids: Vec<u32> = fresh.iter().map(|s| s.id).collect();

    if old_ids != new_ids {
        // Stream set changed — full reconfigure
        info!("Streams changed: {} -> {} streams", assigned.len(), fresh.len());
        apply_stream_config(dev, &fresh)?;
        sync_midi_state(midi_out, &fresh)?;
        *assigned = fresh;
        return Ok(());
    }

    // Same streams — check for media_name changes and update buttons only
    let mut any_changed = false;
    for (i, (old, new)) in assigned.iter().zip(fresh.iter()).enumerate() {
        if old.media_name != new.media_name {
            debug!("Stream {} media_name changed: {:?} -> {:?}", i, old.media_name, new.media_name);
            if !any_changed {
                dev.start_config_update()?;
                any_changed = true;
            }
            dev.send_midi_button_config(&make_button_config(i, new))?;
        }
    }
    if any_changed {
        dev.end_config_update()?;
    }
    // Update media_name in assigned streams
    for (old, new) in assigned.iter_mut().zip(fresh.iter()) {
        old.media_name = new.media_name.clone();
    }

    Ok(())
}

/// Pick a color for a stream: config override > stable hash of app name into pool.
fn pick_color(stream: &pipewire::AudioStream) -> u8 {
    stream.color_scheme.unwrap_or_else(|| {
        let hash = stream.app_name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
        COLOR_POOL[(hash as usize) % COLOR_POOL.len()]
    })
}

fn make_knob_config(i: usize, stream: &pipewire::AudioStream) -> protocol::MidiKnobConfig {
    protocol::MidiKnobConfig {
        setup_index: 0,
        control_index: i as u8,
        control_mode: protocol::ControlMode::Cc7Bit,
        control_channel: midi::MIDI_CHANNEL,
        control_param: midi::KNOB_CC_BASE + i as u8,
        nrpn_address: 0,
        min_value: 0,
        max_value: 127,
        control_name: stream.app_name.clone(),
        color_scheme: pick_color(stream),
        haptic_mode: protocol::KnobHapticMode::Normal,
        haptic_indent1: 0xFF,
        haptic_indent2: 0xFF,
        haptic_steps: 0,
        step_names: vec!["".to_string(); 16],
    }
}

fn make_button_config(i: usize, stream: &pipewire::AudioStream) -> protocol::MidiButtonConfig {
    let has_detail = stream.media_name.is_some();
    let control_name = stream.media_name.clone().unwrap_or_default();
    let color = if has_detail { pick_color(stream) } else { 70 }; // 70 = black/unlit

    protocol::MidiButtonConfig {
        setup_index: 0,
        control_index: i as u8,
        control_mode: protocol::ControlMode::Cc7Bit,
        control_channel: midi::MIDI_CHANNEL,
        control_param: midi::BUTTON_CC_BASE + i as u8,
        nrpn_address: 0xFFFF,
        min_value: 0,
        max_value: 127,
        control_name,
        color_scheme: color,
        led_on_color: 14,  // white
        led_off_color: 70, // dark
        haptic_mode: protocol::SwitchHapticMode::Toggle,
        haptic_steps: 0,
        step_names: vec!["".to_string(); 16],
    }
}

/// Convert a volume float (0.0-1.0) to a MIDI CC value (0-127).
fn volume_to_cc(volume: f64) -> u8 {
    (volume.clamp(0.0, 1.0) * 127.0).round() as u8
}

/// Convert a MIDI CC value (0-127) to a volume float (0.0-1.0).
fn cc_to_volume(cc: u8) -> f64 {
    cc as f64 / 127.0
}

fn find_roto_control_port() -> Option<String> {
    if let Ok(ports) = serialport::available_ports() {
        let mut roto_ports: Vec<_> = ports
            .into_iter()
            .filter(|p| {
                if let serialport::SerialPortType::UsbPort(usb) = &p.port_type {
                    usb.vid == 0x2E8A && usb.pid == 0xF010
                } else {
                    false
                }
            })
            .collect();
        roto_ports.sort_by(|a, b| a.port_name.cmp(&b.port_name));

        debug!(
            "Found Roto-Control ports via serialport: {:?}",
            roto_ports.iter().map(|p| &p.port_name).collect::<Vec<_>>()
        );

        if let Some(p) = roto_ports.into_iter().next() {
            return Some(p.port_name);
        }
    }

    debug!("Falling back to sysfs scan for Roto-Control");
    let mut found: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/tty") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("ttyACM") {
                continue;
            }
            let device_link = entry.path().join("device");
            if let Ok(dev_path) = std::fs::canonicalize(&device_link) {
                let mut p = dev_path.as_path();
                loop {
                    let vid = std::fs::read_to_string(p.join("idVendor"))
                        .unwrap_or_default().trim().to_lowercase();
                    let pid = std::fs::read_to_string(p.join("idProduct"))
                        .unwrap_or_default().trim().to_lowercase();
                    if vid == "2e8a" && pid == "f010" {
                        found.push(format!("/dev/{}", name));
                        break;
                    }
                    match p.parent() {
                        Some(parent) if parent != p => p = parent,
                        _ => break,
                    }
                }
            }
        }
    }
    found.sort();
    debug!("Found Roto-Control ports via sysfs: {:?}", found);
    found.into_iter().next()
}
