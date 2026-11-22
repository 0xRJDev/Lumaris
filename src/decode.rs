// Image decoding on worker threads.
use std::collections::{HashMap, VecDeque};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
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
    "3fr", "ari", "arw", "bay", "cr2", "cr3", "crw", "dcr", "dng", "erf", "fff", "iiq", "k25", "kdc", "mef", "mos", "mrw", "nef", "nrw",
    "orf", "pef", "raf", "raw", "rw2", "rwl", "sr2", "srf", "srw", "x3f",
];

pub fn decode(path: &Path) -> Result<Decoded, String> {
    // a page of a comic archive
    if !path.exists()
        && let Some(bytes) = crate::archive::read(path)
    {
        return decode_bytes(&bytes?);
    }
    match extension(path).as_str() {
        "svg" | "svgz" => decode_svg(path),
        "heic" | "heif" | "hif" => decode_heif(path),
        "jxl" => decode_jxl(path),
        "psd" | "psb" => decode_psd(path),
        "kra" | "ora" => decode_merged_image(path),
        "jp2" | "j2k" | "j2c" | "jpf" | "jpx" | "jpc" => decode_jpeg2000(path),
        "xcf" => decode_xcf(path),
        "pdf" => decode_pdf(path),
        e if RAW_EXTENSIONS.contains(&e) => {
            let develop = crate::settings::current().choice("RawMode") == 1;
            crate::raw::decode(path, develop).map(Decoded::raster)
        }
        _ => decode_image(path),
    }
}

/// An image in memory (pages of archives)
fn decode_bytes(bytes: &[u8]) -> Result<Decoded, String> {
    if let Some(d) = decode_jpeg(bytes) {
        return d;
    }
    let mut reader = ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    reader.no_limits();
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    Ok(Decoded::raster(img))
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
        Some(image::ImageFormat::Jpeg) => {
            if let Some(d) = decode_jpeg(&std::fs::read(path).map_err(|e| e.to_string())?) {
                return d;
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
    let mut d = match img {
        // floating point images (EXR, HDR) hold linear light: tone map to sRGB
        DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_) => tone_map(img.into_rgba32f()),
        img => Decoded::raster(img),
    };
    if let Some(icc) = icc {
        to_srgb(&icc, &mut d.rgba);
    }
    Ok(d)
}

/// Colour JPEGs: decoded straight to RGBA, in parallel bands where the file allows it
/// (crate::jpeg). None: another kind of JPEG, for the general decoder.
fn decode_jpeg(bytes: &[u8]) -> Option<Result<Decoded, String>> {
    if bytes.get(..3)? != [0xFF, 0xD8, 0xFF] {
        return None;
    }
    // orientation and colour profile (headers only)
    let mut header = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let orientation = header.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let icc = if crate::settings::current().bool("ColourProfiles") { header.icc_profile().ok().flatten() } else { None };
    drop(header);
    let (w, h, rgba) = crate::jpeg::decode(bytes)?;
    let mut img = DynamicImage::ImageRgba8(image::RgbaImage::from_raw(w, h, rgba)?);
    img.apply_orientation(orientation);
    let mut d = Decoded::raster(img);
    if let Some(icc) = icc {
        to_srgb(&icc, &mut d.rgba);
    }
    Some(Ok(d))
}

/// Linear light (HDR) to 8 bit sRGB: a soft shoulder keeps highlights instead of clipping them.
fn tone_map(img: image::Rgba32FImage) -> Decoded {
    let (w, h) = img.dimensions();
    let encode = |v: f32| {
        let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
        let v = v / (1.0 + v * 0.15); // gentle compression above 1
        let s = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
        (s.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    };
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for p in img.pixels() {
        rgba.extend_from_slice(&[encode(p.0[0]), encode(p.0[1]), encode(p.0[2]), (p.0[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8]);
    }
    Decoded { width: w, height: h, rgba, frames: Vec::new(), vector: None, pixel_width: w, pixel_height: h }
}

/// Photoshop (PSD/PSB): the flattened image every file contains (the image data section).
/// 8 and 16 bit RGB and grayscale, raw or RLE compressed.
fn decode_psd(path: &Path) -> Result<Decoded, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let bad = || "not a supported Photoshop file".to_string();
    let be16 = |o: usize| data.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize).ok_or_else(bad);
    let be32 = |o: usize| data.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize).ok_or_else(bad);
    if data.get(0..4) != Some(b"8BPS") {
        return Err(bad());
    }
    let psb = be16(4)? == 2;
    let channels = be16(12)?;
    let (h, w) = (be32(14)?, be32(18)?);
    let depth = be16(22)?;
    let mode = be16(24)?; // 1 grayscale, 3 RGB
    if !(depth == 8 || depth == 16) || !(mode == 1 || mode == 3) || w == 0 || h == 0 {
        return Err(bad());
    }
    let mut o = 26;
    o += 4 + be32(o)?; // colour mode data
    o += 4 + be32(o)?; // image resources
    o += if psb { 8 + ((be32(o)? << 32) | be32(o + 4)?) } else { 4 + be32(o)? }; // layers and masks
    let compression = be16(o)?;
    o += 2;
    let bpc = depth / 8;
    let row = w * bpc;
    // planes: one per channel, rows of `row` bytes
    let mut planes = vec![vec![0u8; row * h]; channels];
    match compression {
        0 => {
            for plane in planes.iter_mut() {
                let src = data.get(o..o + row * h).ok_or_else(bad)?;
                plane.copy_from_slice(src);
                o += row * h;
            }
        }
        1 => {
            let count_size = if psb { 4 } else { 2 };
            let counts: Vec<usize> =
                (0..channels * h).map(|i| if psb { be32(o + i * 4) } else { be16(o + i * 2) }).collect::<Result<_, _>>()?;
            o += channels * h * count_size;
            for (c, plane) in planes.iter_mut().enumerate() {
                for y in 0..h {
                    let n = counts[c * h + y];
                    let src = data.get(o..o + n).ok_or_else(bad)?;
                    o += n;
                    // PackBits
                    let dst = &mut plane[y * row..(y + 1) * row];
                    let (mut i, mut j) = (0, 0);
                    while i < src.len() && j < row {
                        let k = src[i] as i8;
                        i += 1;
                        if k >= 0 {
                            let len = (k as usize + 1).min(row - j).min(src.len() - i);
                            dst[j..j + len].copy_from_slice(&src[i..i + len]);
                            i += k as usize + 1;
                            j += len;
                        } else if k != -128 {
                            let len = ((1 - k as isize) as usize).min(row - j);
                            let v = *src.get(i).ok_or_else(bad)?;
                            dst[j..j + len].fill(v);
                            i += 1;
                            j += len;
                        }
                    }
                }
            }
        }
        _ => return Err(bad()),
    }
    let colour = if mode == 3 { 3 } else { 1 };
    let alpha = channels > colour;
    let sample = |plane: &Vec<u8>, i: usize| if bpc == 2 { plane[i * 2] } else { plane[i] }; // high byte for 16 bit
    let mut rgba = Vec::with_capacity(w * h * 4);
    for i in 0..w * h {
        let (r, g, b) = if colour == 3 {
            (sample(&planes[0], i), sample(&planes[1], i), sample(&planes[2], i))
        } else {
            let v = sample(&planes[0], i);
            (v, v, v)
        };
        let a = if alpha { sample(&planes[colour], i) } else { 255 };
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    let (w, h) = (w as u32, h as u32);
    Ok(Decoded { width: w, height: h, rgba, frames: Vec::new(), vector: None, pixel_width: w, pixel_height: h })
}

/// JPEG 2000 (OpenJPEG, ported to Rust)
fn decode_jpeg2000(path: &Path) -> Result<Decoded, String> {
    let img = jpeg2k::Image::from_file(path).map_err(|e| e.to_string())?;
    let img = DynamicImage::try_from(&img).map_err(|e| e.to_string())?;
    Ok(Decoded::raster(img))
}

/// GIMP: the visible layers composited (normal mode, opacity, offsets)
fn decode_xcf(path: &Path) -> Result<Decoded, String> {
    use xcf::PropertyIdentifier as P;
    let file = xcf::Xcf::open(path).map_err(|e| format!("{e:?}"))?;
    let (w, h) = file.dimensions();
    let mut out = vec![0f32; (w * h * 4) as usize]; // premultiplied
    // layers are stored top first
    for layer in file.layers.iter().rev() {
        let prop = |kind: P| {
            layer.properties.iter().find(|p| p.kind == kind).and_then(|p| match &p.payload {
                xcf::PropertyPayload::Unknown(b) => Some(b.clone()),
                _ => None,
            })
        };
        let be32 = |b: &[u8], o: usize| b.get(o..o + 4).map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]));
        if prop(P::PropVisible).and_then(|b| be32(&b, 0)) == Some(0) {
            continue;
        }
        let opacity = prop(P::PropOpacity).and_then(|b| be32(&b, 0)).map(|o| o.min(255) as f32 / 255.0).unwrap_or(1.0);
        let (ox, oy) = prop(P::PropOffsets)
            .map(|b| (be32(&b, 0).unwrap_or(0) as i32 as i64, be32(&b, 4).unwrap_or(0) as i32 as i64))
            .unwrap_or((0, 0));
        let pixels = layer.raw_rgba_buffer();
        for y in 0..layer.height as i64 {
            let ty = y + oy;
            if ty < 0 || ty >= h as i64 {
                continue;
            }
            for x in 0..layer.width as i64 {
                let tx = x + ox;
                if tx < 0 || tx >= w as i64 {
                    continue;
                }
                let p = pixels[(y * layer.width as i64 + x) as usize].0;
                let a = p[3] as f32 / 255.0 * opacity;
                let i = ((ty * w as i64 + tx) * 4) as usize;
                for c in 0..3 {
                    out[i + c] = p[c] as f32 / 255.0 * a + out[i + c] * (1.0 - a);
                }
                out[i + 3] = a + out[i + 3] * (1.0 - a);
            }
        }
    }
    // back to straight alpha
    let mut rgba = Vec::with_capacity(out.len());
    for px in out.as_chunks::<4>().0 {
        let a = px[3];
        let un = |v: f32| if a > 0.0 { (v / a * 255.0 + 0.5).min(255.0) as u8 } else { 0 };
        rgba.extend_from_slice(&[un(px[0]), un(px[1]), un(px[2]), (a * 255.0 + 0.5) as u8]);
    }
    Ok(Decoded { width: w, height: h, rgba, frames: Vec::new(), vector: None, pixel_width: w, pixel_height: h })
}

/// PDF: the first page, rendered by Poppler (pdftocairo) at about twice the screen height
fn decode_pdf(path: &Path) -> Result<Decoded, String> {
    let out = std::process::Command::new("pdftocairo")
        .args(["-png", "-f", "1", "-l", "1", "-singlefile", "-scale-to", "2880"])
        .arg(path)
        .arg("-")
        .output()
        .map_err(|_| "showing PDF files needs poppler (pdftocairo)".to_string())?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let img = image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(Decoded::raster(img))
}

/// Krita and OpenRaster files: zip archives with the merged image as mergedimage.png
fn decode_merged_image(path: &Path) -> Result<Decoded, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let mut entry = zip.by_name("mergedimage.png").map_err(|_| "no merged image in the file".to_string())?;
    let mut png = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut png).map_err(|e| e.to_string())?;
    let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(Decoded::raster(img))
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
    let Ok(t) = lcms2::Transform::<[u8; 4], [u8; 4]>::new(
        &profile,
        lcms2::PixelFormat::RGBA_8,
        &srgb,
        lcms2::PixelFormat::RGBA_8,
        lcms2::Intent::Perceptual,
    ) else {
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
        let ms = num.checked_div(den).unwrap_or(0);
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
    for px in rgba.as_chunks_mut::<4>().0 {
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
    let icc = handle.color_profile_raw().map(|p| p.data);
    let img = lib.decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None).map_err(|e| e.to_string())?;
    let planes = img.planes();
    let plane = planes.interleaved.ok_or("no image data")?;
    let (w, h) = (plane.width, plane.height);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for row in plane.data.chunks(plane.stride).take(h as usize) {
        rgba.extend_from_slice(&row[..(w * 4) as usize]);
    }
    if let (Some(icc), true) = (icc, crate::settings::current().bool("ColourProfiles")) {
        to_srgb(&icc, &mut rgba);
    }
    Ok(Decoded { width: w, height: h, rgba, frames: Vec::new(), vector: None, pixel_width: w, pixel_height: h })
}

fn decode_jxl(path: &Path) -> Result<Decoded, String> {
    let mut decoder = jxl_oxide::integration::JxlDecoder::new(open(path)?).map_err(|e| e.to_string())?;
    let icc = if crate::settings::current().bool("ColourProfiles") { decoder.icc_profile().ok().flatten() } else { None };
    let img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    let mut d = Decoded::raster(img);
    if let Some(icc) = icc {
        to_srgb(&icc, &mut d.rgba);
    }
    Ok(d)
}

/// Largest texture side the GPU takes (0: not known yet). Larger images are reduced on the
/// decoder threads, so showing them does not stall the window.
static MAX_TEXTURE: AtomicU32 = AtomicU32::new(0);

pub fn set_max_texture(side: u32) {
    MAX_TEXTURE.store(side, Ordering::Relaxed);
}

/// Reduces a still raster image to the GPU's texture limit; `width`/`height` keep the real size.
fn fit_to_gpu(d: &mut Decoded) {
    let max = MAX_TEXTURE.load(Ordering::Relaxed);
    if max == 0 || d.vector.is_some() || !d.frames.is_empty() || (d.pixel_width <= max && d.pixel_height <= max) {
        return;
    }
    let Some(src) = image::RgbaImage::from_raw(d.pixel_width, d.pixel_height, std::mem::take(&mut d.rgba)) else { return };
    let s = max as f64 / d.pixel_width.max(d.pixel_height) as f64;
    let (w, h) = (((d.pixel_width as f64 * s) as u32).clamp(1, max), ((d.pixel_height as f64 * s) as u32).clamp(1, max));
    d.rgba = image::imageops::resize(&src, w, h, image::imageops::FilterType::Triangle).into_raw();
    (d.pixel_width, d.pixel_height) = (w, h);
}

/// Decodes on worker threads and sends the results as `UserEvent::Decoded`.
pub fn decode_for_display(path: &Path) -> Result<Arc<Decoded>, String> {
    // a crashing decoder (damaged file) must not end Lumaris
    std::panic::catch_unwind(|| decode(path)).unwrap_or_else(|_| Err("the file could not be decoded".into())).map(|mut d| {
        fit_to_gpu(&mut d);
        Arc::new(d)
    })
}

#[derive(Default)]
struct Queue {
    /// waiting, most wanted first
    waiting: VecDeque<PathBuf>,
    /// being decoded, with the generation they were started in
    running: HashMap<PathBuf, u64>,
    generation: u64,
}

/// Decodes the wanted files on background threads, most wanted first. Each `want` replaces the
/// waiting list, so images skipped past while browsing quickly are never decoded.
pub struct Loader {
    shared: Arc<(Mutex<Queue>, Condvar)>,
}

impl Loader {
    pub fn new(threads: usize, proxy: EventLoopProxy<UserEvent>) -> Loader {
        let shared = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        for _ in 0..threads {
            let shared = shared.clone();
            let proxy = proxy.clone();
            thread::spawn(move || {
                let (lock, wake) = &*shared;
                loop {
                    let (path, generation) = {
                        let mut q = lock.lock().unwrap();
                        loop {
                            if let Some(p) = q.waiting.pop_front() {
                                let g = q.generation;
                                q.running.insert(p.clone(), g);
                                break (p, g);
                            }
                            q = wake.wait(q).unwrap();
                        }
                    };
                    let result = decode_for_display(&path);
                    {
                        let mut q = lock.lock().unwrap();
                        if q.running.get(&path) == Some(&generation) {
                            q.running.remove(&path);
                        }
                    }
                    if proxy.send_event(UserEvent::Decoded(path, generation, result)).is_err() {
                        break;
                    }
                }
            });
        }
        Loader { shared }
    }

    /// The files to decode, most wanted first (replaces what is still waiting). Files being
    /// decoded already are not decoded again.
    pub fn want(&self, paths: Vec<PathBuf>) {
        let (lock, wake) = &*self.shared;
        let mut q = lock.lock().unwrap();
        let generation = q.generation;
        let waiting = paths.into_iter().filter(|p| q.running.get(p) != Some(&generation)).collect();
        q.waiting = waiting;
        wake.notify_all();
    }

    /// Results of decodes started before this are outdated (the settings changed).
    pub fn invalidate(&self) {
        let mut q = self.shared.0.lock().unwrap();
        q.generation += 1;
        q.waiting.clear();
    }

    pub fn generation(&self) -> u64 {
        self.shared.0.lock().unwrap().generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduced_to_gpu_limit() {
        let still = |w: u32, h: u32| Decoded {
            width: w,
            height: h,
            rgba: vec![255; (w * h * 4) as usize],
            frames: Vec::new(),
            vector: None,
            pixel_width: w,
            pixel_height: h,
        };
        set_max_texture(100);
        let mut big = still(300, 50);
        fit_to_gpu(&mut big);
        assert_eq!((big.width, big.height, big.pixel_width, big.pixel_height), (300, 50, 100, 16));
        assert_eq!(big.rgba.len(), 100 * 16 * 4);
        let mut small = still(80, 60);
        fit_to_gpu(&mut small);
        assert_eq!((small.pixel_width, small.pixel_height), (80, 60));
        set_max_texture(0);
    }
}
