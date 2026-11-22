// Shared drawing for Lumaris's own panels (menu, settings, image info): the desktop's font and
// colours (KDE's kdeglobals when available), text rendering and simple shapes.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use resvg::tiny_skia::{Color, PathBuilder, Pixmap};

pub struct Style {
    pub font: Option<fontdue::Font>,
    pub point_size: f32,
    pub background: [u8; 3],
    pub text: [u8; 3],
    pub selection: [u8; 3],
    pub selection_text: [u8; 3],
}

fn kde_color(ini: &HashMap<String, HashMap<String, String>>, group: &str, key: &str, fallback: [u8; 3]) -> [u8; 3] {
    ini.get(group)
        .and_then(|g| g.get(key))
        .and_then(|v| {
            let p: Vec<u8> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (p.len() >= 3).then(|| [p[0], p[1], p[2]])
        })
        .unwrap_or(fallback)
}

fn load_style() -> Style {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    let cfg = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).unwrap_or(home.join(".config"));
    let text = std::fs::read_to_string(cfg.join("kdeglobals")).unwrap_or_default();
    let mut ini: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_string();
        } else if let Some((k, v)) = line.split_once('=') {
            ini.entry(section.clone()).or_default().insert(k.trim().into(), v.trim().into());
        }
    }
    // font: "Family,10,..." -> family and point size
    let (family, point_size) = ini
        .get("General")
        .and_then(|g| g.get("menuFont").or(g.get("font")))
        .map(|f| {
            let mut p = f.split(',');
            (p.next().unwrap_or("sans-serif").to_string(), p.next().and_then(|s| s.parse().ok()).unwrap_or(10.0))
        })
        .unwrap_or(("sans-serif".into(), 10.0));
    let font = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", &family])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|path| std::fs::read(path.trim()).ok())
        .and_then(|data| fontdue::Font::from_bytes(data, fontdue::FontSettings::default()).ok());
    Style {
        font,
        point_size,
        // Breeze Dark as fallback
        background: kde_color(&ini, "Colors:Window", "BackgroundNormal", [32, 35, 38]),
        text: kde_color(&ini, "Colors:Window", "ForegroundNormal", [252, 252, 252]),
        selection: kde_color(&ini, "Colors:Selection", "BackgroundNormal", [61, 174, 233]),
        selection_text: kde_color(&ini, "Colors:Selection", "ForegroundNormal", [252, 252, 252]),
    }
}

pub fn style() -> &'static Style {
    static STYLE: OnceLock<Style> = OnceLock::new();
    STYLE.get_or_init(load_style)
}

/// Loads font and colours in the background (so the first menu opens instantly).
pub fn warm_up() {
    std::thread::spawn(|| {
        style();
    });
}

type GlyphCache = HashMap<(char, u32), (fontdue::Metrics, Arc<Vec<u8>>)>;

pub fn glyph(font: &fontdue::Font, c: char, px: f32) -> (fontdue::Metrics, Arc<Vec<u8>>) {
    static CACHE: OnceLock<Mutex<GlyphCache>> = OnceLock::new();
    let key = (c, px.to_bits());
    let cache = CACHE.get_or_init(Default::default);
    if let Some(g) = cache.lock().unwrap().get(&key) {
        return g.clone();
    }
    let (m, bitmap) = font.rasterize(c, px);
    let g = (m, Arc::new(bitmap));
    cache.lock().unwrap().insert(key, g.clone());
    g
}

pub fn text_width(font: &fontdue::Font, text: &str, px: f32) -> f32 {
    text.chars().map(|c| glyph(font, c, px).0.advance_width).sum()
}

/// Blends text into the premultiplied pixmap at baseline y.
#[allow(clippy::too_many_arguments)] // position, size, colour and opacity belong together here
pub fn draw_text(pm: &mut Pixmap, font: &fontdue::Font, text: &str, px: f32, mut x: f32, baseline: f32, rgb: [u8; 3], alpha: f32) {
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data_mut();
    for c in text.chars() {
        let (m, bitmap) = glyph(font, c, px);
        let gx = (x + m.xmin as f32).round() as i32;
        let gy = (baseline - m.height as f32 - m.ymin as f32).round() as i32;
        for row in 0..m.height as i32 {
            for col in 0..m.width as i32 {
                let (px_x, px_y) = (gx + col, gy + row);
                if px_x < 0 || px_y < 0 || px_x >= w || px_y >= h {
                    continue;
                }
                let cov = bitmap[(row * m.width as i32 + col) as usize] as f32 / 255.0 * alpha;
                if cov <= 0.0 {
                    continue;
                }
                let i = ((px_y * w + px_x) * 4) as usize;
                for k in 0..3 {
                    data[i + k] = (rgb[k] as f32 * cov + data[i + k] as f32 * (1.0 - cov)).round() as u8;
                }
                data[i + 3] = (255.0 * cov + data[i + 3] as f32 * (1.0 - cov)).round() as u8;
            }
        }
        x += m.advance_width;
    }
}

pub fn color(rgb: [u8; 3], a: f32) -> Color {
    Color::from_rgba8(rgb[0], rgb[1], rgb[2], (a.clamp(0.0, 1.0) * 255.0) as u8)
}

pub fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<resvg::tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}
