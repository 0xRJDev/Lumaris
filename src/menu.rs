// The right-click menu, drawn by Lumaris itself (winit has no menus) in the style of the
// desktop: font and colours come from KDE's settings (kdeglobals) when available.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use resvg::tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    OpenWith(usize),
    OpenFolder,
    CopyImage,
    CopyPath,
    Next,
    Previous,
    /// interval in ms, 0 = stop
    Slideshow(u64),
    ToggleLoop,
    Fit,
    ActualSize,
    ZoomIn,
    ZoomOut,
    Fullscreen,
    Quit,
}

#[derive(Clone)]
pub struct Item {
    label: String,
    shortcut: String,
    action: Option<Action>,
    children: Vec<Item>,
    checked: Option<bool>,
    separator: bool,
    enabled: bool,
}

impl Item {
    pub fn new(label: &str, shortcut: &str, action: Action) -> Item {
        Item { label: label.into(), shortcut: shortcut.into(), action: Some(action), children: Vec::new(), checked: None, separator: false, enabled: true }
    }
    pub fn check(label: &str, shortcut: &str, action: Action, checked: bool) -> Item {
        Item { checked: Some(checked), ..Item::new(label, shortcut, action) }
    }
    pub fn sub(label: &str, children: Vec<Item>) -> Item {
        let enabled = !children.is_empty();
        Item { label: label.into(), shortcut: String::new(), action: None, children, checked: None, separator: false, enabled }
    }
    pub fn separator() -> Item {
        Item { separator: true, enabled: false, ..Item::sub("", Vec::new()) }
    }
    pub fn disabled(mut self) -> Item {
        self.enabled = false;
        self
    }
}

// ---------------------------------------------------------------------------------------------
// style

struct Style {
    font: Option<fontdue::Font>,
    point_size: f32,
    background: [u8; 3],
    text: [u8; 3],
    selection: [u8; 3],
    selection_text: [u8; 3],
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

fn style() -> &'static Style {
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

fn glyph(font: &fontdue::Font, c: char, px: f32) -> (fontdue::Metrics, Arc<Vec<u8>>) {
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

fn text_width(font: &fontdue::Font, text: &str, px: f32) -> f32 {
    text.chars().map(|c| glyph(font, c, px).0.advance_width).sum()
}

/// Blends text into the premultiplied pixmap at baseline y.
fn draw_text(pm: &mut Pixmap, font: &fontdue::Font, text: &str, px: f32, mut x: f32, baseline: f32, rgb: [u8; 3], alpha: f32) {
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

fn color(rgb: [u8; 3], a: f32) -> Color {
    Color::from_rgba8(rgb[0], rgb[1], rgb[2], (a.clamp(0.0, 1.0) * 255.0) as u8)
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<resvg::tiny_skia::Path> {
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

// ---------------------------------------------------------------------------------------------
// layout and interaction

struct Level {
    items: Vec<Item>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    tops: Vec<f32>, // item top positions (relative)
    hover: Option<usize>,
    /// the item of the previous level that opened this submenu
    opened_by: Option<usize>,
    pixmap: Option<Pixmap>,
}

pub enum Click {
    Nothing,
    Close,
    Run(Action),
}

pub struct Menu {
    levels: Vec<Level>,
    scale: f32,
    window: (f32, f32),
    /// increases whenever the picture changes
    pub version: u64,
}

impl Menu {
    fn unit(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }
    fn px(&self) -> f32 {
        (style().point_size * 96.0 / 72.0 * self.scale).round()
    }
    fn item_height(&self) -> f32 {
        self.unit(style().point_size * 2.8)
    }

    pub fn open(items: Vec<Item>, at: (f64, f64), window: (u32, u32), scale: f64) -> Menu {
        let mut m = Menu { levels: Vec::new(), scale: scale as f32, window: (window.0 as f32, window.1 as f32), version: 0 };
        m.push_level(items, at.0 as f32, at.1 as f32, None, None);
        m
    }

    /// Adds a menu level at (x, y); `parent` = x range of the parent level for submenus.
    fn push_level(&mut self, items: Vec<Item>, x: f32, y: f32, parent: Option<(f32, f32)>, opened_by: Option<usize>) {
        let st = style();
        let px = self.px();
        let pad = self.unit(4.0);
        let ih = self.item_height();
        let sep_h = self.unit(9.0);
        let (mut lw, mut sw) = (0.0f32, 0.0f32);
        let mut tops = Vec::new();
        let mut h = pad;
        for it in &items {
            tops.push(h);
            h += if it.separator { sep_h } else { ih };
            if let Some(f) = &st.font {
                lw = lw.max(text_width(f, &it.label, px));
                sw = sw.max(text_width(f, &it.shortcut, px));
            }
        }
        h += pad;
        let w = (pad + self.unit(30.0) + lw + if sw > 0.0 { self.unit(36.0) + sw } else { 0.0 } + self.unit(30.0) + pad)
            .max(self.unit(160.0))
            .ceil();
        let (ww, wh) = self.window;
        let x = match parent {
            // submenu: right of the parent, or left when there is no room
            Some((left, right)) => {
                if right - pad + w <= ww || left + pad - w < 0.0 { right - pad } else { left + pad - w }
            }
            None => if x + w > ww { (x - w).max(0.0) } else { x },
        };
        let x = x.min(ww - w).max(0.0).round();
        let y = (if y + h > wh { (wh - h).max(0.0) } else { y }).round();
        self.levels.push(Level { items, x, y, w, h: h.ceil(), tops, hover: None, opened_by, pixmap: None });
        self.version += 1;
    }

    fn hit(&self, p: (f64, f64)) -> Option<(usize, Option<usize>)> {
        let (x, y) = (p.0 as f32, p.1 as f32);
        for (li, l) in self.levels.iter().enumerate().rev() {
            if x >= l.x && x < l.x + l.w && y >= l.y && y < l.y + l.h {
                let ih = self.item_height();
                let sep_h = self.unit(9.0);
                let item = l.items.iter().enumerate().position(|(i, it)| {
                    let top = l.y + l.tops[i];
                    y >= top && y < top + if it.separator { sep_h } else { ih }
                });
                return Some((li, item));
            }
        }
        None
    }

    fn set_hover(&mut self, li: usize, item: Option<usize>) {
        let item = item.filter(|&i| self.levels[li].items[i].enabled && !self.levels[li].items[i].separator);
        if self.levels[li].hover != item {
            self.levels[li].hover = item;
            self.levels[li].pixmap = None;
            self.version += 1;
        }
        // submenus
        let wants = item.filter(|&i| !self.levels[li].items[i].children.is_empty());
        let open = self.levels.get(li + 1).and_then(|l| l.opened_by);
        if open != wants {
            self.levels.truncate(li + 1);
            self.version += 1;
            if let Some(i) = wants {
                let l = &self.levels[li];
                let (children, y, range) = (l.items[i].children.clone(), l.y + l.tops[i] - self.unit(4.0), (l.x, l.x + l.w));
                self.push_level(children, 0.0, y, Some(range), Some(i));
            }
        }
    }

    /// Mouse moved. Returns true when the menu must be redrawn.
    pub fn hover(&mut self, p: (f64, f64)) -> bool {
        let v = self.version;
        if let Some((li, item)) = self.hit(p) {
            self.set_hover(li, item);
        }
        v != self.version
    }

    pub fn click(&mut self, p: (f64, f64)) -> Click {
        match self.hit(p) {
            None => Click::Close,
            Some((li, Some(i))) => {
                let it = &self.levels[li].items[i];
                match (it.enabled, it.action) {
                    (true, Some(a)) => Click::Run(a),
                    _ => Click::Nothing,
                }
            }
            Some(_) => Click::Nothing,
        }
    }

    /// Keyboard: Up/Down/Left/Right/Enter. Returns the action for Enter.
    pub fn key(&mut self, key: &str) -> Click {
        let li = self.levels.len() - 1;
        let n = self.levels[li].items.len();
        let usable = |l: &Level, i: usize| l.items[i].enabled && !l.items[i].separator;
        match key {
            "Down" | "Up" => {
                let l = &self.levels[li];
                let start = l.hover.unwrap_or(if key == "Down" { n - 1 } else { 0 });
                let mut i = start;
                for _ in 0..n {
                    i = if key == "Down" { (i + 1) % n } else { (i + n - 1) % n };
                    if usable(l, i) {
                        break;
                    }
                }
                if usable(&self.levels[li], i) {
                    self.levels[li].hover = Some(i);
                    self.levels[li].pixmap = None;
                    self.version += 1;
                }
                Click::Nothing
            }
            "Right" | "Enter" => {
                let Some(i) = self.levels[li].hover else { return Click::Nothing };
                let it = &self.levels[li].items[i];
                if let (true, Some(a)) = (it.enabled, it.action) {
                    return if key == "Enter" { Click::Run(a) } else { Click::Nothing };
                }
                if !it.children.is_empty() {
                    self.set_hover(li, Some(i));
                    let next = li + 1;
                    if next < self.levels.len() {
                        let first = (0..self.levels[next].items.len()).find(|&k| usable(&self.levels[next], k));
                        self.levels[next].hover = first;
                        self.levels[next].pixmap = None;
                        self.version += 1;
                    }
                }
                Click::Nothing
            }
            "Left" => {
                if li > 0 {
                    self.levels.truncate(li);
                    self.version += 1;
                }
                Click::Nothing
            }
            _ => Click::Nothing,
        }
    }

    /// The menu levels as premultiplied RGBA images: (pixels, width, height, x, y)
    pub fn layers(&mut self) -> Vec<(&[u8], u32, u32, i32, i32)> {
        for i in 0..self.levels.len() {
            if self.levels[i].pixmap.is_none() {
                let pm = self.paint(i);
                self.levels[i].pixmap = pm;
            }
        }
        self.levels
            .iter()
            .filter_map(|l| l.pixmap.as_ref().map(|p| (p.data(), p.width(), p.height(), l.x as i32, l.y as i32)))
            .collect()
    }

    fn paint(&self, li: usize) -> Option<Pixmap> {
        let st = style();
        let l = &self.levels[li];
        let mut pm = Pixmap::new(l.w as u32, l.h as u32)?;
        let (w, h) = (l.w, l.h);
        let r = self.unit(6.0);
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        if let Some(path) = rounded_rect(0.5, 0.5, w - 1.0, h - 1.0, r) {
            paint.set_color(color(st.background, 0.97));
            pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
            paint.set_color(color(st.text, 0.18));
            pm.stroke_path(&path, &paint, &Stroke { width: 1.0, ..Default::default() }, Transform::identity(), None);
        }
        let px = self.px();
        let pad = self.unit(4.0);
        let ih = self.item_height();
        let sep_h = self.unit(9.0);
        for (i, it) in l.items.iter().enumerate() {
            let top = l.tops[i];
            if it.separator {
                let y = (top + sep_h / 2.0).floor();
                paint.set_color(color(st.text, 0.15));
                if let Some(rect) = Rect::from_xywh(pad + self.unit(8.0), y, w - 2.0 * pad - self.unit(16.0), 1.0) {
                    pm.fill_rect(rect, &paint, Transform::identity(), None);
                }
                continue;
            }
            let hovered = l.hover == Some(i);
            if hovered {
                if let Some(path) = rounded_rect(pad, top, w - 2.0 * pad, ih, self.unit(3.0)) {
                    paint.set_color(color(st.selection, 0.35));
                    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
                    paint.set_color(color(st.selection, 0.9));
                    pm.stroke_path(&path, &paint, &Stroke { width: 1.0, ..Default::default() }, Transform::identity(), None);
                }
            }
            let fg = if hovered { st.selection_text } else { st.text };
            let alpha = if it.enabled { 1.0 } else { 0.45 };
            if it.checked == Some(true) {
                // check mark
                let (cx, cy, s) = (pad + self.unit(15.0), top + ih / 2.0, self.unit(5.0));
                let mut pb = PathBuilder::new();
                pb.move_to(cx - s, cy);
                pb.line_to(cx - s * 0.3, cy + s * 0.7);
                pb.line_to(cx + s, cy - s * 0.8);
                if let Some(path) = pb.finish() {
                    paint.set_color(color(fg, alpha));
                    pm.stroke_path(&path, &paint, &Stroke { width: self.unit(1.6), ..Default::default() }, Transform::identity(), None);
                }
            }
            if !it.children.is_empty() {
                // submenu arrow
                let (cx, cy, s) = (w - pad - self.unit(14.0), top + ih / 2.0, self.unit(4.0));
                let mut pb = PathBuilder::new();
                pb.move_to(cx - s * 0.5, cy - s);
                pb.line_to(cx + s * 0.5, cy);
                pb.line_to(cx - s * 0.5, cy + s);
                if let Some(path) = pb.finish() {
                    paint.set_color(color(fg, alpha));
                    pm.stroke_path(&path, &paint, &Stroke { width: self.unit(1.4), ..Default::default() }, Transform::identity(), None);
                }
            }
            if let Some(font) = &st.font {
                let lm = font.horizontal_line_metrics(px);
                let (asc, desc) = lm.map(|m| (m.ascent, m.descent)).unwrap_or((px * 0.8, -px * 0.2));
                let baseline = (top + (ih - (asc - desc)) / 2.0 + asc).round();
                draw_text(&mut pm, font, &it.label, px, pad + self.unit(30.0), baseline, fg, alpha);
                if !it.shortcut.is_empty() {
                    let sw = text_width(font, &it.shortcut, px);
                    draw_text(&mut pm, font, &it.shortcut, px, w - pad - self.unit(30.0) - sw, baseline, fg, alpha * 0.6);
                }
            }
        }
        Some(pm)
    }
}
