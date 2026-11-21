// Image decoding on worker threads.
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageReader};
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

/// Animations larger than this (all frames together) show only their first frame.
const MAX_ANIMATION_BYTES: usize = 1 << 30;
/// Vector images are rendered at most at this many pixels (re-rendered when zooming).
pub const MAX_VECTOR_PIXELS: f64 = 64.0 * 1024.0 * 1024.0;

/// One frame of an animation (fully composited, RGBA8) and how long it is shown.
pub struct Frame {
    pub rgba: Vec<u8>,
    pub delay: Duration,
}

/// A decoded image: straight alpha RGBA8, orientation applied.
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Animations: all frames (the first one is also in `rgba`)
    pub frames: Vec<Frame>,
    /// Vector images (SVG): the document, for sharp re-rendering at other sizes.
    /// `width`/`height` are its natural size; `rgba` may be rendered at another size
    /// (`pixel_width` x `pixel_height`).
    pub vector: Option<Arc<resvg::usvg::Tree>>,
    pub pixel_width: u32,
    pub pixel_height: u32,
}

impl Decoded {
    fn raster(img: DynamicImage) -> Decoded {
        let rgba = img.into_rgba8();
        let (width, height) = rgba.dimensions();
        Decoded { width, height, rgba: rgba.into_raw(), frames: Vec::new(), vector: None, pixel_width: width, pixel_height: height }
    }
}

fn extension(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

pub const RAW_EXTENSIONS: &[&str] = &[
    "3fr", "ari", "arw", "bay", "cr2", "cr3", "crw", "dcr", "dng", "erf", "fff", "iiq", "k25", "kdc", "mef", "mos", "mrw",
    "nef", "nrw", "orf", "pef", "raf", "raw", "rw2", "rwl", "sr2", "srf", "srw", "x3f",
];

pub fn decode(path: &Path) -> Result<Decoded, String> {
    match extension(path).as_str() {
        "svg" | "svgz" => decode_svg(path),
        "heic" | "heif" | "hif" => decode_heif(path),
        "jxl" => decode_jxl(path),
        e if RAW_EXTENSIONS.contains(&e) => {
            let develop = crate::settings::current().choice("RawMode") == 1;
            crate::raw::decode(path, develop).map(Decoded::raster)
        }
        _ => decode_image(path),
    }
}

fn open(path: &Path) -> Result<BufReader<std::fs::File>, String> {
    std::fs::File::open(path).map(BufReader::new).map_err(|e| e.to_string())
}

/// Formats of the image crate (JPEG, PNG/APNG, GIF, WebP, AVIF, BMP, TIFF, ...)
fn decode_image(path: &Path) -> Result<Decoded, String> {
    let mut reader = ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
    reader.no_limits();
    // animations
    match reader.format() {
        Some(image::ImageFormat::Gif) => {
            return animation(image::codecs::gif::GifDecoder::new(open(path)?).map_err(|e| e.to_string())?);
        }
        Some(image::ImageFormat::WebP) => {
            let dec = image::codecs::webp::WebPDecoder::new(open(path)?).map_err(|e| e.to_string())?;
            if dec.has_animation() {
                return animation(dec);
            }
        }
        Some(image::ImageFormat::Png) => {
            let dec = image::codecs::png::PngDecoder::new(open(path)?).map_err(|e| e.to_string())?;
            if dec.is_apng().unwrap_or(false) {
                return animation(dec.apng().map_err(|e| e.to_string())?);
            }
        }
        _ => {}
    }
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let icc = if crate::settings::current().bool("ColourProfiles") { decoder.icc_profile().ok().flatten() } else { None };
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    let mut d = Decoded::raster(img);
    if let Some(icc) = icc {
        to_srgb(&icc, &mut d.rgba);
    }
    Ok(d)
}

/// Converts RGBA8 pixels from the embedded colour profile to sRGB (Little CMS).
/// sRGB profiles and profiles of other colour spaces (e.g. CMYK) are left alone.
fn to_srgb(icc: &[u8], rgba: &mut [u8]) {
    let Ok(profile) = lcms2::Profile::new_icc(icc) else { return };
    if profile.color_space() != lcms2::ColorSpaceSignature::RgbData {
        return;
    }
    let name = profile.info(lcms2::InfoType::Description, lcms2::Locale::none()).unwrap_or_default();
    if name.to_ascii_lowercase().contains("srgb") {
        return;
    }
    let srgb = lcms2::Profile::new_srgb();
    let Ok(t) = lcms2::Transform::<[u8; 4], [u8; 4]>::new(&profile, lcms2::PixelFormat::RGBA_8, &srgb, lcms2::PixelFormat::RGBA_8, lcms2::Intent::Perceptual)
    else {
        return;
    };
    t.transform_in_place(bytemuck::cast_slice_mut(rgba));
}

fn animation<'a>(dec: impl AnimationDecoder<'a>) -> Result<Decoded, String> {
    let mut frames = Vec::new();
    let mut bytes = 0usize;
    let mut size = (0, 0);
    for f in dec.into_frames() {
        let f = f.map_err(|e| e.to_string())?;
        let (num, den) = f.delay().numer_denom_ms();
        let ms = if den == 0 { 0 } else { num / den };
        // browsers show very short delays (0..10 ms) as 100 ms
        let delay = Duration::from_millis(if ms <= 10 { 100 } else { ms as u64 });
        let buf = f.into_buffer();
        size = buf.dimensions();
        bytes += buf.len();
        frames.push(Frame { rgba: buf.into_raw(), delay });
        if bytes > MAX_ANIMATION_BYTES {
            frames.truncate(1);
            break;
        }
    }
    if frames.is_empty() {
        return Err("no frames".into());
    }
    let rgba = frames[0].rgba.clone();
    if frames.len() == 1 {
        frames.clear();
    }
    Ok(Decoded { width: size.0, height: size.1, rgba, frames, vector: None, pixel_width: size.0, pixel_height: size.1 })
}

/// System fonts (for text in SVG files), loaded once on first use
pub fn fonts() -> Arc<resvg::usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = resvg::usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

fn decode_svg(path: &Path) -> Result<Decoded, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let opt = resvg::usvg::Options { resources_dir: path.parent().map(|p| p.to_path_buf()), fontdb: fonts(), ..Default::default() };
    let tree = Arc::new(resvg::usvg::Tree::from_data(&data, &opt).map_err(|e| e.to_string())?);
    let size = tree.size();
    let (w, h) = ((size.width().ceil() as u32).max(1), (size.height().ceil() as u32).max(1));
    render_svg(tree, w, h, 1.0)
}

/// Renders a vector image at `scale` times its natural size (limited to MAX_VECTOR_PIXELS)
pub fn render_svg(tree: Arc<resvg::usvg::Tree>, width: u32, height: u32, scale: f64) -> Result<Decoded, String> {
    let limit = (MAX_VECTOR_PIXELS / (width as f64 * height as f64)).sqrt();
    let s = scale.min(limit).max(0.01);
    let (pw, ph) = (((width as f64 * s).round() as u32).max(1), ((height as f64 * s).round() as u32).max(1));
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pw, ph).ok_or("image too large")?;
    let ts = resvg::tiny_skia::Transform::from_scale(pw as f32 / tree.size().width(), ph as f32 / tree.size().height());
    resvg::render(&tree, ts, &mut pixmap.as_mut());
    // tiny-skia is premultiplied, the renderer expects straight alpha
    let mut rgba = pixmap.take();
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Ok(Decoded { width, height, rgba, frames: Vec::new(), vector: Some(tree), pixel_width: pw, pixel_height: ph })
}

fn decode_heif(path: &Path) -> Result<Decoded, String> {
    use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};
    let lib = LibHeif::new();
    let ctx = HeifContext::read_from_file(path.to_str().ok_or("path")?).map_err(|e| e.to_string())?;
    let handle = ctx.primary_image_handle().map_err(|e| e.to_string())?;
    // libheif applies the orientation (irot/imir) while decoding
    let img = lib.decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None).map_err(|e| e.to_string())?;
    let planes = img.planes();
    let plane = planes.interleaved.ok_or("no image data")?;
    let (w, h) = (plane.width, plane.height);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for row in plane.data.chunks(plane.stride).take(h as usize) {
        rgba.extend_from_slice(&row[..(w * 4) as usize]);
    }
    Ok(Decoded { width: w, height: h, rgba, frames: Vec::new(), vector: None, pixel_width: w, pixel_height: h })
}

fn decode_jxl(path: &Path) -> Result<Decoded, String> {
    let decoder = jxl_oxide::integration::JxlDecoder::new(open(path)?).map_err(|e| e.to_string())?;
    let img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    Ok(Decoded::raster(img))
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
