// Image decoding on worker threads.
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use image::{DynamicImage, ImageDecoder, ImageReader};
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

/// A decoded image: straight alpha RGBA8, EXIF orientation applied.
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn decode(path: &Path) -> Result<Decoded, String> {
    let mut reader = ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    reader.no_limits();
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    let rgba = img.into_rgba8();
    Ok(Decoded { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
}

/// Decodes requested files on background threads; results arrive as `UserEvent::Decoded`.
pub struct Loader {
    tx: mpsc::Sender<PathBuf>,
}

impl Loader {
    pub fn new(threads: usize, proxy: EventLoopProxy<UserEvent>) -> Loader {
        let (tx, rx) = mpsc::channel::<PathBuf>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..threads {
            let rx = rx.clone();
            let proxy = proxy.clone();
            thread::spawn(move || loop {
                let next = rx.lock().unwrap().recv();
                let Ok(path) = next else { break };
                let result = decode(&path).map(Arc::new);
                if proxy.send_event(UserEvent::Decoded(path, result)).is_err() {
                    break;
                }
            });
        }
        Loader { tx }
    }

    pub fn request(&self, path: PathBuf) {
        let _ = self.tx.send(path);
    }
}
