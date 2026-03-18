//! System tray icon using StatusNotifierItem (ksni).

use ksni::blocking::TrayMethods;
use log::warn;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

const ICON_PNG: &[u8] = include_bytes!("../../melbourne.png");

struct RotoTray {
    shutdown: Arc<AtomicBool>,
}

impl ksni::Tray for RotoTray {
    fn id(&self) -> String {
        "rotocontrol".into()
    }

    fn title(&self) -> String {
        "Rotool".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        match decode_png_to_argb32(ICON_PNG) {
            Some(icon) => vec![icon],
            None => vec![],
        }
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let shutdown = self.shutdown.clone();
        vec![
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
    let decoder = png::Decoder::new(png_data);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
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
