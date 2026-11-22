// Desktop integration: clipboard (wl-copy on Wayland, xclip on X11) and showing a file in the
// file manager (org.freedesktop.FileManager1).
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use crate::decode::Decoded;

/// Copies still being prepared (waited for at exit, so a copy right before quitting works)
static COPIES: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
static COPIES_DONE: std::sync::Condvar = std::sync::Condvar::new();

fn copy_in_background(f: impl FnOnce() + Send + 'static) {
    *COPIES.lock().unwrap() += 1;
    std::thread::spawn(move || {
        f();
        *COPIES.lock().unwrap() -= 1;
        COPIES_DONE.notify_all();
    });
}

/// Waits (at most a few seconds) until running copies have reached the clipboard
pub fn wait_for_copies() {
    let mut n = COPIES.lock().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while *n > 0 {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        n = COPIES_DONE.wait_timeout(n, left).unwrap().0;
    }
}

fn clipboard_command(mime: &str) -> Command {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        let mut c = Command::new("wl-copy");
        c.args(["--type", mime]);
        c
    } else {
        let mut c = Command::new("xclip");
        c.args(["-selection", "clipboard", "-t", mime]);
        c
    }
}

fn to_clipboard(mime: &str, data: &[u8]) {
    let mut cmd = clipboard_command(mime);
    match cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(mut child) => {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(data);
            }
            let _ = child.wait();
        }
        Err(e) => eprintln!("lumaris: cannot copy to the clipboard ({:?} is needed): {e}", cmd.get_program()),
    }
}

/// Copies the whole image (full size) to the clipboard as PNG, in the background.
pub fn copy_image(img: Arc<Decoded>) {
    copy_in_background(move || {
        let mut png = Vec::new();
        let enc = image::codecs::png::PngEncoder::new_with_quality(
            &mut png,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::Adaptive,
        );
        use image::ImageEncoder;
        if enc.write_image(&img.rgba, img.pixel_width, img.pixel_height, image::ExtendedColorType::Rgba8).is_ok() {
            to_clipboard("image/png", &png);
        }
    });
}

pub fn copy_text(text: String) {
    copy_in_background(move || to_clipboard("text/plain;charset=utf-8", text.as_bytes()));
}

/// Opens the file manager with the file selected (falls back to opening the folder).
pub fn show_in_folder(file: PathBuf) {
    std::thread::spawn(move || {
        let uri = crate::apps::file_uri(&file);
        let shown = zbus::blocking::Connection::session().ok().and_then(|c| {
            c.call_method(
                Some("org.freedesktop.FileManager1"),
                "/org/freedesktop/FileManager1",
                Some("org.freedesktop.FileManager1"),
                "ShowItems",
                &(vec![uri.as_str()], ""),
            )
            .ok()
        });
        if shown.is_none()
            && let Some(dir) = file.parent()
        {
            crate::apps::spawn_detached(Command::new("xdg-open").arg(dir));
        }
    });
}
