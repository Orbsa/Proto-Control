mod config;
mod discord;
mod midi;
mod pipewire;
mod protocol;
mod tray;

use anyhow::{Context, Result};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_KNOBS: usize = 8;
const MAX_CONTROLS: usize = 32; // 4 pages × 8 controls per setup
/// Fallback poll interval in case the pactl watcher misses an event.
const STREAM_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Color scheme indices chosen to be visually distinct across the 85-entry palette.
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
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

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

    // 2. Switch to MIDI mode and set setup names
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

    // 4. Configure page 1 (PipeWire streams)
    let mut assigned_streams: Vec<pipewire::AudioStream> = streams.into_iter().take(MAX_CONTROLS).collect();
    apply_stream_config(&mut dev, &assigned_streams, 0)?;

    // 5. Open MIDI connections
    info!("Opening MIDI connections...");
    let mut midi_out = midi::open_output()?;
    let (_midi_in_conn, midi_rx) = midi::open_input()?;

    // 6. Set initial knob positions to match current volumes
    std::thread::sleep(Duration::from_millis(200));
    sync_midi_state(&mut midi_out, &assigned_streams)?;

    // 7. Start Discord integration on page 2 (if configured)
    let discord_handle = if let Some(ref dc) = config.discord {
        dev.set_setup_name(1, "Discord")?;
        info!("Starting Discord voice integration...");
        Some(discord::start(dc.client_id.clone(), dc.client_secret.clone()))
    } else {
        None
    };
    let mut discord_members: Vec<discord::VoiceMember> = vec![];

    // 8. Start tray icon
    let tray_shutdown = shutdown.clone();
    let _tray_handle = tray::spawn(tray_shutdown);

    // 9. Start PipeWire event watcher
    let pw_events = pipewire::watch_changes();

    info!("Ready! Turn knobs to adjust volume, press buttons to mute/unmute.");

    // 10. Main event loop
    let mut last_scan = Instant::now();
    // Remember last known volume/mute per app so we can restore it when a stream restarts
    let mut volume_memory: HashMap<String, (f64, bool)> = HashMap::new();
    // Seed memory with current state
    for stream in &assigned_streams {
        volume_memory.insert(stream.app_name.clone(), (stream.volume, stream.muted));
    }
    // Pending latest values per control index (None = no pending change)
    let mut pending_knobs: [Option<u8>; MAX_KNOBS] = [None; MAX_KNOBS];
    let mut pending_buttons: [Option<u8>; MAX_KNOBS] = [None; MAX_KNOBS];
    let mut pending_discord_knobs: [Option<u8>; MAX_KNOBS] = [None; MAX_KNOBS];
    let mut pending_discord_buttons: [Option<u8>; MAX_KNOBS] = [None; MAX_KNOBS];

    while !shutdown.load(Ordering::SeqCst) {
        // Wait for at least one event, then drain all pending to batch
        let first = midi_rx.recv_timeout(Duration::from_millis(100));
        let events: Vec<_> = match first {
            Ok(evt) => {
                let mut batch = vec![evt];
                while let Ok(e) = midi_rx.try_recv() {
                    batch.push(e);
                }
                batch
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => vec![],
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                warn!("MIDI input disconnected");
                break;
            }
        };

        // Coalesce: keep only the latest value per control
        for evt in events {
            match evt {
                midi::DeviceEvent::KnobTurn { index, value } if index < MAX_KNOBS => {
                    pending_knobs[index] = Some(value);
                }
                midi::DeviceEvent::ButtonPress { index, value } if index < MAX_KNOBS => {
                    pending_buttons[index] = Some(value);
                }
                midi::DeviceEvent::DiscordKnobTurn { index, value } if index < MAX_KNOBS => {
                    pending_discord_knobs[index] = Some(value);
                }
                midi::DeviceEvent::DiscordButtonPress { index, value } if index < MAX_KNOBS => {
                    pending_discord_buttons[index] = Some(value);
                }
                _ => {}
            }
        }

        // Apply coalesced PipeWire knob changes
        for index in 0..MAX_KNOBS {
            if let Some(value) = pending_knobs[index].take() {
                if index < assigned_streams.len() {
                    let stream = &mut assigned_streams[index];
                    let new_volume = cc_to_volume(value);
                    debug!("Knob {}: {} -> {:.0}%", index, stream.app_name, new_volume * 100.0);
                    if let Err(e) = pipewire::set_volume(stream.id, new_volume) {
                        warn!("Failed to set volume for {}: {}", stream.app_name, e);
                    }
                    stream.volume = new_volume;
                    volume_memory.insert(stream.app_name.clone(), (stream.volume, stream.muted));
                }
            }
            if let Some(value) = pending_buttons[index].take() {
                if index < assigned_streams.len() {
                    let stream = &mut assigned_streams[index];
                    let now_muted = value > 0;
                    debug!("Button {}: {} muted={}", index, stream.app_name, now_muted);
                    if now_muted != stream.muted {
                        if let Err(e) = pipewire::toggle_mute(stream.id) {
                            warn!("Failed to toggle mute for {}: {}", stream.app_name, e);
                        }
                        stream.muted = now_muted;
                        volume_memory.insert(stream.app_name.clone(), (stream.volume, stream.muted));
                    }
                }
            }
        }

        // Apply coalesced Discord changes
        if let Some(ref handle) = discord_handle {
            for index in 0..MAX_KNOBS {
                if let Some(value) = pending_discord_knobs[index].take() {
                    if index < discord_members.len() {
                        let member = &mut discord_members[index];
                        let new_volume = cc_to_discord_volume(value);
                        debug!("Discord knob {}: {} -> vol {}", index, member.nick, new_volume);
                        let _ = handle.cmd_tx.send(discord::Command::SetVolume {
                            user_id: member.user_id.clone(),
                            volume: new_volume,
                        });
                        member.volume = new_volume;
                    }
                }
                if let Some(value) = pending_discord_buttons[index].take() {
                    if index < discord_members.len() {
                        let member = &mut discord_members[index];
                        let now_muted = value > 0;
                        debug!("Discord button {}: {} muted={}", index, member.nick, now_muted);
                        if now_muted != member.muted {
                            let _ = handle.cmd_tx.send(discord::Command::SetMute {
                                user_id: member.user_id.clone(),
                                muted: now_muted,
                            });
                            member.muted = now_muted;
                        }
                    }
                }
            }
        }

        // Check for Discord member updates
        if let Some(ref handle) = discord_handle {
            while let Ok(members) = handle.members_rx.try_recv() {
                let fresh: Vec<_> = members.into_iter().take(MAX_KNOBS).collect();
                let old_ids: Vec<&str> = discord_members.iter().map(|m| m.user_id.as_str()).collect();
                let new_ids: Vec<&str> = fresh.iter().map(|m| m.user_id.as_str()).collect();

                if old_ids != new_ids {
                    let prev_len = discord_members.len();
                    info!("Discord members changed: {} -> {} members", prev_len, fresh.len());
                    if let Err(e) = apply_discord_config(&mut dev, &fresh, prev_len) {
                        warn!("Failed to configure Discord page: {}", e);
                    }
                    if let Err(e) = sync_discord_midi_state(&mut midi_out, &fresh) {
                        warn!("Failed to sync Discord MIDI state: {}", e);
                    }
                }
                discord_members = fresh;
            }
        }

        // Rescan PipeWire streams on events or fallback timer
        let pw_changed = pw_events.try_recv().is_ok();
        // Drain any additional events that arrived in the same batch
        while pw_events.try_recv().is_ok() {}
        if pw_changed || last_scan.elapsed() >= STREAM_POLL_INTERVAL {
            last_scan = Instant::now();
            if let Err(e) = rescan_streams(
                &mut dev,
                &mut assigned_streams,
                &mut midi_out,
                &config,
                &volume_memory,
            ) {
                warn!("Stream rescan failed: {}", e);
            }
        }
    }

    // 10. Cleanup: clear both pages
    info!("Shutting down, clearing display...");
    if let Err(e) = dev.clear_all() {
        warn!("Failed to clear display: {}", e);
    }

    Ok(())
}

// ---- Page 1: PipeWire stream config ----

fn apply_stream_config(dev: &mut protocol::Device, streams: &[pipewire::AudioStream], prev_count: usize) -> Result<()> {
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
    // Only clear slots that were previously occupied but no longer are
    for i in streams.len()..prev_count {
        dev.send_clear_knob(0, i as u8)?;
        dev.send_clear_button(0, i as u8)?;
    }
    dev.end_config_update()?;
    Ok(())
}

fn sync_midi_state(midi_out: &mut midir::MidiOutputConnection, streams: &[pipewire::AudioStream]) -> Result<()> {
    for (i, stream) in streams.iter().enumerate() {
        let cc_value = volume_to_cc(stream.volume);
        midi::send_knob_value(midi_out, i, cc_value)?;
        midi::send_button_value(midi_out, i, if stream.muted { 127 } else { 0 })?;
    }
    Ok(())
}

fn rescan_streams(
    dev: &mut protocol::Device,
    assigned: &mut Vec<pipewire::AudioStream>,
    midi_out: &mut midir::MidiOutputConnection,
    config: &config::Config,
    volume_memory: &HashMap<String, (f64, bool)>,
) -> Result<()> {
    let fresh = pipewire::list_streams(config)?;
    let mut fresh: Vec<_> = fresh.into_iter().take(MAX_CONTROLS).collect();

    let old_ids: Vec<u32> = assigned.iter().map(|s| s.id).collect();
    let new_ids: Vec<u32> = fresh.iter().map(|s| s.id).collect();

    if old_ids != new_ids {
        let prev_count = assigned.len();
        // Restore remembered volumes for any streams whose node ID changed
        for stream in &mut fresh {
            let is_new_id = !old_ids.contains(&stream.id);
            if is_new_id {
                if let Some(&(vol, muted)) = volume_memory.get(&stream.app_name) {
                    if (stream.volume - vol).abs() > 0.005 || stream.muted != muted {
                        info!("Restoring volume for {} -> {:.0}%{}", stream.app_name, vol * 100.0,
                            if muted { " MUTED" } else { "" });
                        let _ = pipewire::set_volume(stream.id, vol);
                        if muted != stream.muted {
                            let _ = pipewire::toggle_mute(stream.id);
                        }
                        stream.volume = vol;
                        stream.muted = muted;
                    }
                }
            }
        }
        info!("Streams changed: {} -> {} streams", prev_count, fresh.len());
        apply_stream_config(dev, &fresh, prev_count)?;
        sync_midi_state(midi_out, &fresh)?;
        *assigned = fresh;
        return Ok(());
    }

    let mut display_changed = false;
    for (i, (old, new)) in assigned.iter().zip(fresh.iter()).enumerate() {
        if old.media_name != new.media_name {
            debug!("Stream {} media_name changed: {:?} -> {:?}", i, old.media_name, new.media_name);
            if !display_changed {
                dev.start_config_update()?;
                display_changed = true;
            }
            dev.send_midi_button_config(&make_button_config(i, new))?;
        }
    }
    if display_changed {
        dev.end_config_update()?;
    }

    // Sync encoder positions for any externally-changed volumes/mutes
    for (i, (old, new)) in assigned.iter_mut().zip(fresh.iter()).enumerate() {
        let vol_delta = (old.volume - new.volume).abs();
        if vol_delta > 0.005 {
            debug!("Stream {} volume drifted: {:.2} -> {:.2}", i, old.volume, new.volume);
            midi::send_knob_value(midi_out, i, volume_to_cc(new.volume))?;
            old.volume = new.volume;
        }
        if old.muted != new.muted {
            debug!("Stream {} mute drifted: {} -> {}", i, old.muted, new.muted);
            midi::send_button_value(midi_out, i, if new.muted { 127 } else { 0 })?;
            old.muted = new.muted;
        }
        old.media_name = new.media_name.clone();
    }

    Ok(())
}

fn pick_color_from_name(name: &str) -> u8 {
    let hash = name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    COLOR_POOL[(hash as usize) % COLOR_POOL.len()]
}

fn pick_color(stream: &pipewire::AudioStream) -> u8 {
    stream.color_scheme.unwrap_or_else(|| pick_color_from_name(&stream.app_name))
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
        led_on_color: 14,
        led_off_color: 70,
        haptic_mode: protocol::SwitchHapticMode::Toggle,
        haptic_steps: 0,
        step_names: vec!["".to_string(); 16],
    }
}

// ---- Page 2: Discord voice config ----

fn apply_discord_config(dev: &mut protocol::Device, members: &[discord::VoiceMember], prev_count: usize) -> Result<()> {
    dev.start_config_update()?;
    for (i, member) in members.iter().enumerate() {
        let nick = pipewire::truncate_to_chars(&member.nick, 12);
        let color = pick_color_from_name(&member.nick);

        info!("Discord knob {}: {} (vol={}, muted={})", i + 1, nick, member.volume, member.muted);

        dev.send_midi_knob_config(&protocol::MidiKnobConfig {
            setup_index: 1,
            control_index: i as u8,
            control_mode: protocol::ControlMode::Cc7Bit,
            control_channel: midi::MIDI_CHANNEL,
            control_param: midi::DISCORD_KNOB_CC_BASE + i as u8,
            nrpn_address: 0,
            min_value: 0,
            max_value: 127,
            control_name: nick,
            color_scheme: color,
            haptic_mode: protocol::KnobHapticMode::CentreIndent,
            haptic_indent1: 0xFF,
            haptic_indent2: 0xFF,
            haptic_steps: 0,
            step_names: vec!["".to_string(); 16],
        })?;

        dev.send_midi_button_config(&protocol::MidiButtonConfig {
            setup_index: 1,
            control_index: i as u8,
            control_mode: protocol::ControlMode::Cc7Bit,
            control_channel: midi::MIDI_CHANNEL,
            control_param: midi::DISCORD_BUTTON_CC_BASE + i as u8,
            nrpn_address: 0xFFFF,
            min_value: 0,
            max_value: 127,
            control_name: String::new(),
            color_scheme: 70, // black/unlit
            led_on_color: 14,
            led_off_color: 70,
            haptic_mode: protocol::SwitchHapticMode::Toggle,
            haptic_steps: 0,
            step_names: vec!["".to_string(); 16],
        })?;
    }
    for i in members.len()..prev_count {
        dev.send_clear_knob(1, i as u8)?;
        dev.send_clear_button(1, i as u8)?;
    }
    dev.end_config_update()?;
    Ok(())
}

fn sync_discord_midi_state(midi_out: &mut midir::MidiOutputConnection, members: &[discord::VoiceMember]) -> Result<()> {
    for (i, member) in members.iter().enumerate() {
        let cc_value = discord_volume_to_cc(member.volume);
        midi::send_discord_knob_value(midi_out, i, cc_value)?;
        midi::send_discord_button_value(midi_out, i, if member.muted { 127 } else { 0 })?;
    }
    Ok(())
}

// ---- Volume conversions ----

/// PipeWire: CC 0-127 maps to volume 0.0-1.0
fn volume_to_cc(volume: f64) -> u8 {
    (volume.clamp(0.0, 1.0) * 127.0).round() as u8
}

fn cc_to_volume(cc: u8) -> f64 {
    cc as f64 / 127.0
}

/// Discord: CC 0-127 maps to volume 0-200
fn discord_volume_to_cc(volume: u16) -> u8 {
    ((volume.min(200) as f64 / 200.0) * 127.0).round() as u8
}

fn cc_to_discord_volume(cc: u8) -> u16 {
    ((cc as f64 / 127.0) * 200.0).round() as u16
}

// ---- Device discovery ----

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
