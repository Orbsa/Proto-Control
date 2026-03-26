//! System tray icon using StatusNotifierItem (ksni).

use ksni::blocking::TrayMethods;
use log::warn;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

#[cfg(has_tray_icon)]
const ICON_PNG: Option<&[u8]> = Some(include_bytes!("../assets/tray.png"));
#[cfg(not(has_tray_icon))]
const ICON_PNG: Option<&[u8]> = None;

struct RotoTray {
    shutdown: Arc<AtomicBool>,
}

impl ksni::Tray for RotoTray {
    fn id(&self) -> String {
        "proto-control".into()
    }

    fn title(&self) -> String {
        "Proto-Control".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        ICON_PNG
            .and_then(decode_png_to_argb32)
            .map(|icon| vec![icon])
            .unwrap_or_default()
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let shutdown = self.shutdown.clone();
        vec![
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Settings".into(),
                activate: Box::new(|_tray: &mut Self| {
                    // Spawn settings GUI as a child process.
                    // Re-exec ourselves with --settings so the GUI runs in its
                    // own process and a display-server failure cannot crash the daemon.
                    match std::env::current_exe() {
                        Ok(exe) => {
                            match std::process::Command::new(exe)
                                .arg("--settings")
                                .stdin(std::process::Stdio::null())
                                .stdout(std::process::Stdio::null())
                                .stderr(std::process::Stdio::null())
                                .spawn()
                            {
                                Ok(_) => {}
                                Err(e) => warn!("Failed to launch settings GUI: {}", e),
                            }
                        }
                        Err(e) => warn!("Failed to determine executable path: {}", e),
                    }
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Quit".into(),
                activate: Box::new(move |_tray: &mut Self| {
                    shutdown.store(true, Ordering::SeqCst);
                }),
                ..Default::default()
            }),
        ]
    }
}

/// Spawn the tray icon on a background thread. Returns the join handle.
pub fn spawn(shutdown: Arc<AtomicBool>) -> Option<JoinHandle<()>> {
    let handle = std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || {
            let tray = RotoTray { shutdown };
            if let Err(e) = tray.spawn() {
                warn!("Tray icon failed: {}", e);
            }
        })
        .ok();
    handle
}

/// Decode a PNG image to ksni::Icon (ARGB32 network byte order).
fn decode_png_to_argb32(png_data: &[u8]) -> Option<ksni::Icon> {
    let decoder = png::Decoder::new(std::io::Cursor::new(png_data));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let rgba = &buf[..info.buffer_size()];

    // Convert RGBA to ARGB (network byte order)
    let mut argb = Vec::with_capacity(rgba.len());
    for pixel in rgba.chunks_exact(4) {
        argb.push(pixel[3]); // A
        argb.push(pixel[0]); // R
        argb.push(pixel[1]); // G
        argb.push(pixel[2]); // B
    }

    Some(ksni::Icon {
        width: info.width as i32,
        height: info.height as i32,
        data: argb,
    })
}
