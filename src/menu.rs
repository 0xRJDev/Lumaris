// The right-click menu, drawn by Lumaris itself (winit has no menus) in the style of the
// desktop: font and colours come from KDE's settings (kdeglobals) when available.
use resvg::tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

use crate::ui::{color, draw_text, rounded_rect, style, text_width};

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
    ImageInfo,
    Settings,
    /// slideshow with the default interval
    SlideshowStart,
    /// an option of a choice list (settings window)
    Choice(usize),
    Open,
    Trash,
    RotateRight,
    RotateLeft,
    FlipHorizontal,
    FlipVertical,
    NavigationPanel,
    Help,
    UserCommand(usize),
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
        Item {
            label: label.into(),
            shortcut: shortcut.into(),
            action: Some(action),
            children: Vec::new(),
            checked: None,
            separator: false,
            enabled: true,
        }
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
    /// where the menu may be placed (window coordinates): the window, or the whole screen
    /// area when the menu is shown on popup surfaces
    bounds: (f32, f32, f32, f32),
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

    /// Like `open`, but the menu may extend beyond the window within `bounds`
    pub fn open_in(items: Vec<Item>, at: (f64, f64), window: (u32, u32), bounds: (f32, f32, f32, f32), scale: f64) -> Menu {
        let mut m = Menu { levels: Vec::new(), scale: scale as f32, window: (window.0 as f32, window.1 as f32), bounds, version: 0 };
        m.push_level(items, at.0 as f32, at.1 as f32, None, None);
        m
    }

    pub fn open(items: Vec<Item>, at: (f64, f64), window: (u32, u32), scale: f64) -> Menu {
        let (ww, wh) = (window.0 as f32, window.1 as f32);
        let mut m = Menu { levels: Vec::new(), scale: scale as f32, window: (ww, wh), bounds: (0.0, 0.0, ww, wh), version: 0 };
        m.push_level(items, at.0 as f32, at.1 as f32, None, None);
        m
    }

    /// Adds a menu level at (x, y); `parent` = x range of the parent level for submenus.
    /// Size of a menu level and the top of each item
    fn measure(&self, items: &[Item]) -> (f32, f32, Vec<f32>) {
        let st = style();
        let px = self.px();
        let pad = self.unit(4.0);
        let ih = self.item_height();
        let sep_h = self.unit(9.0);
        let (mut lw, mut sw) = (0.0f32, 0.0f32);
        let mut tops = Vec::new();
        let mut h = pad;
        for it in items {
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
        (w, h.ceil(), tops)
    }

    /// Window size needed to show the menu with its widest submenu beside it
    pub fn needed_size(items: &[Item], scale: f64) -> (u32, u32) {
        let m = Menu {
            levels: Vec::new(),
            scale: scale as f32,
            window: (f32::MAX, f32::MAX),
            bounds: (0.0, 0.0, f32::MAX, f32::MAX),
            version: 0,
        };
        let (w, h, tops) = m.measure(items);
        let (mut sub_w, mut bottom) = (0.0f32, h);
        for (it, top) in items.iter().zip(&tops) {
            if !it.children.is_empty() {
                let (cw, ch, _) = m.measure(&it.children);
                sub_w = sub_w.max(cw);
                bottom = bottom.max(top + ch);
            }
        }
        let margin = m.unit(8.0);
        ((w + sub_w + margin * 2.0) as u32, (bottom + margin * 2.0) as u32)
    }

    fn push_level(&mut self, items: Vec<Item>, x: f32, y: f32, parent: Option<(f32, f32)>, opened_by: Option<usize>) {
        let pad = self.unit(4.0);
        let (w, h, tops) = self.measure(&items);
        let (bx0, by0, bx1, by1) = self.bounds;
        let x = match parent {
            // submenu: right of the parent, or left when there is no room
            Some((left, right)) => {
                if right - pad + w <= bx1 || left + pad - w < bx0 {
                    right - pad
                } else {
                    left + pad - w
                }
            }
            None => {
                if x + w > bx1 {
                    (x - w).max(bx0)
                } else {
                    x
                }
            }
        };
        let x = x.min(bx1 - w).max(bx0).round();
        let y = (if y + h > by1 { (by1 - h).max(by0) } else { y }).max(by0).round();
        self.levels.push(Level { items, x, y, w, h, tops, hover: None, opened_by, pixmap: None });
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

    /// true when the menu may extend beyond the window (shown on popup surfaces)
    pub fn on_popups(&self) -> bool {
        self.bounds != (0.0, 0.0, self.window.0, self.window.1)
    }

    /// Positions and sizes of the open levels (tests)
    pub fn rects(&self) -> Vec<(i32, i32, i32, i32)> {
        self.levels.iter().map(|l| (l.x as i32, l.y as i32, l.w as i32, l.h as i32)).collect()
    }

    /// The window size the menu was laid out for
    pub fn window_size(&self) -> (u32, u32) {
        (self.window.0 as u32, self.window.1 as u32)
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
        self.levels.iter().filter_map(|l| l.pixmap.as_ref().map(|p| (p.data(), p.width(), p.height(), l.x as i32, l.y as i32))).collect()
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
            if hovered && let Some(path) = rounded_rect(pad, top, w - 2.0 * pad, ih, self.unit(3.0)) {
                paint.set_color(color(st.selection, 0.35));
                pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
                paint.set_color(color(st.selection, 0.9));
                pm.stroke_path(&path, &paint, &Stroke { width: 1.0, ..Default::default() }, Transform::identity(), None);
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
