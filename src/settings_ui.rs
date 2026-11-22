// The settings window, drawn inside the Lumaris window: sections on the left, the settings of
// the selected section on the right. Changes apply immediately. The last section is the Keys
// page: click a key to change it, x to remove it, + to add one.
use resvg::tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::keys::{COMMANDS, KeyMap};
use crate::menu::{self, Action, Item, Menu};
use crate::settings::{DEFS, Def, Kind, SECTIONS, Settings, Value};
use crate::ui::{color, draw_text, rounded_rect, style, text_width};

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Section(usize),
    Close,
    EditFile(u8),
    Toggle(usize),
    Minus(usize),
    Plus(usize),
    Choice(usize),
    /// a key of a command (index in COMMANDS, key number)
    KeyChip(usize, usize),
    KeyRemove(usize, usize),
    KeyAdd(usize),
    ResetKeys,
    DialogYes,
    DialogNo,
}

/// The Keys page: after the sections of the settings
pub const KEYS: usize = SECTIONS.len();

fn section_name(i: usize) -> &'static str {
    SECTIONS.get(i).copied().unwrap_or("Keys")
}

/// A question or notice over the Keys page
struct Dialog {
    text: String,
    /// the key, the command and key number it is for, and the command that has it now
    swap: Option<(String, usize, Option<usize>, usize)>,
}

pub enum Outcome {
    Nothing,
    Redraw,
    Changed(Settings),
    KeysChanged(KeyMap),
    Close,
    EditFile(u8),
}

pub struct SettingsPanel {
    values: Settings,
    section: usize,
    scale: f32,
    window: (f32, f32),
    /// where the panel may be: the window, or the screen around it when it is shown on popup
    /// surfaces (window coordinates: left, top, right, bottom)
    bounds: (f32, f32, f32, f32),
    rect: (f32, f32, f32, f32),
    /// dragged by its header: pointer position relative to the panel
    drag: Option<(f32, f32)>,
    hits: Vec<(Hit, f32, f32, f32, f32)>,
    pixmap: Option<Pixmap>,
    /// open choice list: setting index and its menu
    choice: Option<(usize, Menu)>,
    scroll: f32,
    max_scroll: f32,
    keymap: KeyMap,
    /// keys of the user commands (name, key): they cannot be given to a command
    user_keys: Vec<(String, String)>,
    /// waiting for a key: command (index in COMMANDS), key number (None: a new key)
    capture: Option<(usize, Option<usize>)>,
    dialog: Option<Dialog>,
}

impl SettingsPanel {
    pub fn new(
        values: Settings,
        user_keys: Vec<(String, String)>,
        window: (u32, u32),
        bounds: (f32, f32, f32, f32),
        scale: f64,
    ) -> SettingsPanel {
        let mut p = SettingsPanel {
            values,
            keymap: crate::keys::current().clone(),
            user_keys,
            capture: None,
            dialog: None,
            section: 0,
            scale: scale as f32,
            window: (0.0, 0.0),
            bounds: (0.0, 0.0, 0.0, 0.0),
            rect: (0.0, 0.0, 0.0, 0.0),
            drag: None,
            hits: Vec::new(),
            pixmap: None,
            choice: None,
            scroll: 0.0,
            max_scroll: 0.0,
        };
        p.resize(window, bounds);
        p
    }

    /// Shows the Keys page
    pub fn open_keys(&mut self) {
        self.section = KEYS;
        self.scroll = 0.0;
        self.pixmap = None;
    }

    pub fn on_keys(&self) -> bool {
        self.section == KEYS
    }

    /// Waiting for a key, or a question is open: keys go to `key`
    pub fn wants_keys(&self) -> bool {
        self.capture.is_some() || self.dialog.is_some()
    }

    /// A key pressed while waiting for one (or while a question is open)
    pub fn key(&mut self, key: &Key, mods: ModifiersState) -> Outcome {
        if let Some(d) = &self.dialog {
            return match key {
                Key::Named(NamedKey::Escape) => self.close_dialog(),
                Key::Named(NamedKey::Enter) if d.swap.is_some() => self.confirm(),
                Key::Named(NamedKey::Enter) => self.close_dialog(),
                _ => Outcome::Nothing,
            };
        }
        let Some((c, slot)) = self.capture else { return Outcome::Nothing };
        if *key == Key::Named(NamedKey::Escape) {
            self.capture = None;
            self.pixmap = None;
            return Outcome::Redraw;
        }
        let Some(spec) = KeyMap::spec_for(key, mods) else { return Outcome::Nothing }; // a modifier alone
        let shown = KeyMap::display(&spec);
        let bare = spec.rsplit('+').next().unwrap_or("");
        if matches!(bare, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9") && !mods.alt_key() {
            return self.notice(format!("{shown} starts a slideshow (1 to 9 always do). Choose another key."));
        }
        if let Some((name, _)) = self.user_keys.iter().find(|(_, k)| KeyMap::same_key(k, &spec)) {
            return self.notice(format!("{shown} runs your command \u{201c}{name}\u{201d} (commands.conf). Choose another key."));
        }
        match self.keymap.command_with(&spec) {
            Some(other) if other == COMMANDS[c].0 => {
                self.capture = None; // it already has this key
                self.pixmap = None;
                Outcome::Redraw
            }
            Some(other) => {
                let o = COMMANDS.iter().position(|e| e.0 == other).unwrap_or(0);
                let what = if slot.is_some() { "Swap the keys?" } else { "Move it here?" };
                self.dialog = Some(Dialog {
                    text: format!("{shown} is used for \u{201c}{}\u{201d}. {what}", COMMANDS[o].2),
                    swap: Some((spec, c, slot, o)),
                });
                self.pixmap = None;
                Outcome::Redraw
            }
            None => self.assign(c, slot, spec, None),
        }
    }

    fn notice(&mut self, text: String) -> Outcome {
        self.capture = None;
        self.dialog = Some(Dialog { text, swap: None });
        self.pixmap = None;
        Outcome::Redraw
    }

    fn close_dialog(&mut self) -> Outcome {
        self.dialog = None;
        self.capture = None;
        self.pixmap = None;
        Outcome::Redraw
    }

    fn confirm(&mut self) -> Outcome {
        match self.dialog.take().and_then(|d| d.swap) {
            Some((spec, c, slot, other)) => self.assign(c, slot, spec, Some(other)),
            None => self.close_dialog(),
        }
    }

    /// Gives command `c` the key (replacing key number `slot`, or as a new key). `other`: the
    /// command that had it; it gets the replaced key instead (swap), or loses the key.
    fn assign(&mut self, c: usize, slot: Option<usize>, spec: String, other: Option<usize>) -> Outcome {
        let cmd = COMMANDS[c].0;
        let mut keys = self.keymap.keys(cmd);
        let replaced = match slot {
            Some(k) if k < keys.len() => Some(std::mem::replace(&mut keys[k], spec.clone())),
            _ => {
                keys.push(spec.clone());
                None
            }
        };
        if let Some(o) = other {
            let ocmd = COMMANDS[o].0;
            let mut theirs = self.keymap.keys(ocmd);
            if let Some(pos) = theirs.iter().position(|k| KeyMap::same_key(k, &spec)) {
                match replaced {
                    Some(r) if !theirs.iter().any(|k| KeyMap::same_key(k, &r)) => theirs[pos] = r,
                    _ => {
                        theirs.remove(pos);
                    }
                }
            }
            self.keymap.set_keys(ocmd, &theirs);
        }
        self.keymap.set_keys(cmd, &keys);
        self.capture = None;
        self.dialog = None;
        self.pixmap = None;
        Outcome::KeysChanged(self.keymap.clone())
    }

    fn u(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }

    /// New window size (and area the panel may use). The panel opens centred on the window and
    /// keeps its place afterwards, inside the bounds.
    pub fn resize(&mut self, window: (u32, u32), bounds: (f32, f32, f32, f32)) {
        self.window = (window.0 as f32, window.1 as f32);
        self.bounds = bounds;
        let (ww, wh) = self.window;
        let (bw, bh) = (bounds.2 - bounds.0, bounds.3 - bounds.1);
        let w = (bw - self.u(24.0)).min(self.u(860.0)).max(1.0).floor();
        let h = (bh - self.u(24.0)).min(self.u(560.0)).max(1.0).floor();
        let (x, y) = if self.rect.2 == 0.0 { ((ww - w) / 2.0, (wh - h) / 2.0) } else { (self.rect.0, self.rect.1) };
        let (x, y) = self.clamp(x, y, w, h);
        self.rect = (x, y, w, h);
        self.pixmap = None;
        self.choice = None;
    }

    fn clamp(&self, x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
        let (b0, b1, b2, b3) = self.bounds;
        (x.min(b2 - w).max(b0).round(), y.min(b3 - h).max(b1).round())
    }

    /// Shown beyond the window (on popup surfaces)
    pub fn on_popups(&self) -> bool {
        self.bounds != (0.0, 0.0, self.window.0, self.window.1)
    }

    /// A press on the header (not on a button) starts dragging the panel.
    pub fn start_drag(&mut self, p: (f64, f64)) -> bool {
        let (rx, ry, rw, _) = self.rect;
        let (x, y) = (p.0 as f32, p.1 as f32);
        if self.dialog.is_some() || x < rx || x >= rx + rw || y < ry || y >= ry + self.u(64.0) || self.hit(p).is_some() {
            return false;
        }
        self.drag = Some((x - rx, y - ry));
        self.choice = None;
        true
    }

    /// Moves the panel while dragging. Returns true when it moved.
    pub fn drag_to(&mut self, p: (f64, f64)) -> bool {
        let Some((ox, oy)) = self.drag else { return false };
        let (rx, ry, w, h) = self.rect;
        let (x, y) = self.clamp(p.0 as f32 - ox, p.1 as f32 - oy, w, h);
        let (dx, dy) = (x - rx, y - ry);
        if dx == 0.0 && dy == 0.0 {
            return false;
        }
        self.rect = (x, y, w, h);
        for hit in &mut self.hits {
            hit.1 += dx;
            hit.2 += dy;
        }
        true
    }

    pub fn end_drag(&mut self) -> bool {
        self.drag.take().is_some()
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// State for test scripts
    pub fn describe(&self) -> String {
        format!(
            "rect={:?} popups={} section={} waiting={:?} dialog={:?}",
            self.rect,
            self.on_popups(),
            section_name(self.section),
            self.capture,
            self.dialog.as_ref().map(|d| d.text.clone())
        )
    }

    /// Size the window should have at least to show the panel (window pixels)
    pub fn wanted_size(scale: f64) -> (u32, u32) {
        ((884.0 * scale) as u32, (584.0 * scale) as u32)
    }

    fn defs(&self) -> impl Iterator<Item = (usize, &'static Def)> {
        let section = section_name(self.section);
        DEFS.iter().enumerate().filter(move |(_, d)| d.section == section)
    }

    fn hit(&self, p: (f64, f64)) -> Option<Hit> {
        let (x, y) = (p.0 as f32, p.1 as f32);
        self.hits.iter().find(|(_, hx, hy, hw, hh)| x >= *hx && x < hx + hw && y >= *hy && y < hy + hh).map(|h| h.0)
    }

    pub fn hover(&mut self, p: (f64, f64)) -> bool {
        if let Some((_, m)) = self.choice.as_mut() {
            return m.hover(p);
        }
        false
    }

    pub fn click(&mut self, p: (f64, f64)) -> Outcome {
        if let Some((i, m)) = self.choice.as_mut() {
            let i = *i;
            return match m.click(p) {
                menu::Click::Run(Action::Choice(k)) => {
                    self.choice = None;
                    self.set(i, Value::Choice(k))
                }
                menu::Click::Close => {
                    self.choice = None;
                    Outcome::Redraw
                }
                _ => Outcome::Nothing,
            };
        }
        if self.dialog.is_some() {
            return match self.hit(p) {
                Some(Hit::DialogYes) => self.confirm(),
                Some(Hit::DialogNo) => self.close_dialog(),
                _ => Outcome::Nothing,
            };
        }
        let (rx, ry, rw, rh) = self.rect;
        let (x, y) = (p.0 as f32, p.1 as f32);
        if x < rx || y < ry || x >= rx + rw || y >= ry + rh {
            return Outcome::Close;
        }
        if self.capture.take().is_some() {
            // a click while waiting for a key: stop waiting
            self.pixmap = None;
            return Outcome::Redraw;
        }
        match self.hit(p) {
            Some(Hit::KeyChip(c, k)) => {
                self.capture = Some((c, Some(k)));
                self.pixmap = None;
                Outcome::Redraw
            }
            Some(Hit::KeyAdd(c)) => {
                self.capture = Some((c, None));
                self.pixmap = None;
                Outcome::Redraw
            }
            Some(Hit::KeyRemove(c, k)) => {
                let cmd = COMMANDS[c].0;
                let mut keys = self.keymap.keys(cmd);
                if k < keys.len() {
                    keys.remove(k);
                }
                self.keymap.set_keys(cmd, &keys);
                self.pixmap = None;
                Outcome::KeysChanged(self.keymap.clone())
            }
            Some(Hit::ResetKeys) => {
                self.keymap = KeyMap::defaults();
                self.pixmap = None;
                Outcome::KeysChanged(self.keymap.clone())
            }
            Some(Hit::DialogYes | Hit::DialogNo) => Outcome::Nothing,
            Some(Hit::Section(s)) => {
                self.section = s;
                self.scroll = 0.0;
                self.pixmap = None;
                Outcome::Redraw
            }
            Some(Hit::Close) => Outcome::Close,
            Some(Hit::EditFile(w)) => Outcome::EditFile(w),
            Some(Hit::Toggle(i)) => {
                let v = matches!(self.values.get(i), Value::Bool(true));
                self.set(i, Value::Bool(!v))
            }
            Some(h @ (Hit::Minus(i) | Hit::Plus(i))) => {
                if let (Kind::Number { min, max, step, .. }, Value::Number(n)) = (&DEFS[i].kind, self.values.get(i)) {
                    let n = if matches!(h, Hit::Plus(_)) { n + step } else { n - step };
                    self.set(i, Value::Number(n.clamp(*min, *max)))
                } else {
                    Outcome::Nothing
                }
            }
            Some(Hit::Choice(i)) => {
                if let (Kind::Choice(options), Value::Choice(cur)) = (&DEFS[i].kind, self.values.get(i)) {
                    let items = options.iter().enumerate().map(|(k, o)| Item::check(o, "", Action::Choice(k), k == cur)).collect();
                    let (_, hx, hy, _, hh) = *self.hits.iter().find(|h| h.0 == Hit::Choice(i)).unwrap();
                    let (at, window) = ((hx as f64, (hy + hh) as f64), (self.window.0 as u32, self.window.1 as u32));
                    let m = if self.on_popups() {
                        Menu::open_in(items, at, window, self.bounds, self.scale as f64)
                    } else {
                        Menu::open(items, at, window, self.scale as f64)
                    };
                    self.choice = Some((i, m));
                }
                Outcome::Redraw
            }
            None => Outcome::Nothing,
        }
    }

    /// Mouse wheel: scrolls the settings. Returns true when the panel must be redrawn.
    pub fn wheel(&mut self, steps: f64) -> bool {
        if self.choice.is_some() {
            return false;
        }
        let old = self.scroll;
        self.scroll = (self.scroll - steps as f32 * self.u(60.0)).clamp(0.0, self.max_scroll);
        if self.scroll != old {
            self.pixmap = None;
            return true;
        }
        false
    }

    /// Esc: closes an open choice list first. Returns true when the panel should close.
    pub fn escape(&mut self) -> bool {
        self.choice.take().is_none()
    }

    fn set(&mut self, i: usize, v: Value) -> Outcome {
        if self.values.get(i) == v {
            return Outcome::Nothing;
        }
        self.values.set(i, v);
        self.pixmap = None;
        Outcome::Changed(self.values.clone())
    }

    pub fn layers(&mut self) -> Vec<(&[u8], u32, u32, i32, i32)> {
        if self.pixmap.is_none() {
            self.pixmap = self.paint();
        }
        let (rx, ry, _, _) = self.rect;
        let mut out: Vec<(&[u8], u32, u32, i32, i32)> = Vec::new();
        if let Some(p) = &self.pixmap {
            out.push((p.data(), p.width(), p.height(), rx as i32, ry as i32));
        }
        if let Some((_, m)) = self.choice.as_mut() {
            out.extend(m.layers());
        }
        out
    }

    fn paint(&mut self) -> Option<Pixmap> {
        let st = style();
        let font = st.font.as_ref()?;
        let (rx, ry, w, h) = self.rect;
        let mut pm = Pixmap::new(w as u32, h as u32)?;
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        let px = (st.point_size * 96.0 / 72.0 * self.scale).round();
        let small = (px * 0.85).round();
        let title = (px * 1.45).round();
        let metrics = |size: f32| font.horizontal_line_metrics(size).map(|m| (m.ascent, m.descent)).unwrap_or((size * 0.8, -size * 0.2));
        let baseline_in = |top: f32, height: f32, size: f32| {
            let (a, d) = metrics(size);
            (top + (height - (a - d)) / 2.0 + a).round()
        };
        let mut hits = Vec::new();
        let fill = |pm: &mut Pixmap, paint: &mut Paint, path: Option<resvg::tiny_skia::Path>, c, a| {
            if let Some(path) = path {
                paint.set_color(color(c, a));
                pm.fill_path(&path, paint, FillRule::Winding, Transform::identity(), None);
            }
        };
        let stroke = |pm: &mut Pixmap, paint: &mut Paint, path: Option<resvg::tiny_skia::Path>, c, a, width: f32| {
            if let Some(path) = path {
                paint.set_color(color(c, a));
                pm.stroke_path(&path, paint, &Stroke { width, ..Default::default() }, Transform::identity(), None);
            }
        };

        // frame and sidebar
        fill(&mut pm, &mut paint, rounded_rect(0.5, 0.5, w - 1.0, h - 1.0, self.u(8.0)), st.background, 1.0);
        let side = self.u(200.0);
        fill(&mut pm, &mut paint, rounded_rect(0.5, 0.5, side, h - 1.0, self.u(8.0)), st.text, 0.04);
        stroke(&mut pm, &mut paint, rounded_rect(0.5, 0.5, w - 1.0, h - 1.0, self.u(8.0)), st.text, 0.18, 1.0);
        draw_text(&mut pm, font, "Settings", title, self.u(20.0), baseline_in(self.u(14.0), self.u(40.0), title), st.text, 1.0);
        let item_h = self.u(34.0);
        for i in 0..=KEYS {
            let name = section_name(i);
            let top = self.u(64.0) + item_h * i as f32;
            if i == self.section {
                let r = rounded_rect(self.u(8.0), top, side - self.u(16.0), item_h, self.u(4.0));
                fill(&mut pm, &mut paint, r.clone(), st.selection, 0.35);
                stroke(&mut pm, &mut paint, r, st.selection, 0.9, 1.0);
            }
            draw_text(&mut pm, font, name, px, self.u(20.0), baseline_in(top, item_h, px), st.text, 1.0);
            hits.push((Hit::Section(i), rx + self.u(8.0), ry + top, side - self.u(16.0), item_h));
        }
        // the files
        for (k, link) in ["Edit settings file…", "Edit key bindings…", "Edit user commands…"].iter().enumerate() {
            let link_top = h - self.u(32.0) * (3 - k) as f32 - self.u(12.0);
            draw_text(&mut pm, font, link, small, self.u(20.0), baseline_in(link_top, self.u(32.0), small), st.selection, 1.0);
            hits.push((
                Hit::EditFile(k as u8),
                rx + self.u(12.0),
                ry + link_top,
                text_width(font, link, small) + self.u(16.0),
                self.u(32.0),
            ));
        }

        // close button
        let (cx, cy, cs) = (w - self.u(30.0), self.u(30.0), self.u(6.0));
        let mut pb = PathBuilder::new();
        pb.move_to(cx - cs, cy - cs);
        pb.line_to(cx + cs, cy + cs);
        pb.move_to(cx + cs, cy - cs);
        pb.line_to(cx - cs, cy + cs);
        stroke(&mut pm, &mut paint, pb.finish(), st.text, 0.8, self.u(1.6));
        hits.push((Hit::Close, rx + cx - self.u(16.0), ry + cy - self.u(16.0), self.u(32.0), self.u(32.0)));

        // settings of the section
        let left = side + self.u(28.0);
        let right = w - self.u(28.0);
        draw_text(&mut pm, font, section_name(self.section), title, left, baseline_in(self.u(14.0), self.u(40.0), title), st.text, 1.0);
        if self.section == KEYS {
            let reset = "Reset to defaults";
            let tw = text_width(font, reset, small);
            let tx = w - self.u(56.0) - tw;
            draw_text(&mut pm, font, reset, small, tx, baseline_in(self.u(14.0), self.u(40.0), small), st.selection, 1.0);
            hits.push((Hit::ResetKeys, rx + tx - self.u(8.0), ry + self.u(14.0), tw + self.u(16.0), self.u(40.0)));
        }
        // the settings rows scroll inside this area
        let area_top = self.u(64.0);
        let area_h = (h - area_top - self.u(12.0)).max(1.0);
        let mut cpm = Pixmap::new(w as u32, area_h as u32)?;
        let key_row_h = self.u(44.0);
        let notes = [
            ("1 \u{2026} 9", "Slideshow, seconds per image (Ctrl: tenths)"),
            ("Mouse wheel", "Zoom (Ctrl: next / previous image)"),
            ("Drag", "Move the image"),
            ("Double click", "Full screen"),
            ("Right click", "Menu"),
            ("Mouse back / forward", "Previous / next image"),
        ];
        let content_h = if self.section == KEYS {
            self.u(6.0) + key_row_h * (COMMANDS.len() + notes.len()) as f32 + self.u(18.0)
        } else {
            self.u(6.0) + self.u(64.0) * self.defs().count() as f32 + self.u(6.0)
        };
        self.max_scroll = (content_h - area_h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, self.max_scroll);
        let screen_top = ry + area_top;
        // a hit area in content coordinates -> window coordinates, cut to the visible part
        let push_hit = |hits: &mut Vec<(Hit, f32, f32, f32, f32)>, hit: Hit, x: f32, y: f32, hw: f32, hh: f32| {
            let (y0, y1) = (y.max(0.0), (y + hh).min(area_h));
            if y1 > y0 {
                hits.push((hit, x, screen_top + y0, hw, y1 - y0));
            }
        };
        if self.section == KEYS {
            let mid_of = |top: f32| top + key_row_h / 2.0;
            for (c, (cmd, _, desc, _)) in COMMANDS.iter().enumerate() {
                let top = self.u(6.0) + key_row_h * c as f32 - self.scroll;
                if top + key_row_h < 0.0 || top > area_h {
                    continue;
                }
                if c > 0 {
                    paint.set_color(color(st.text, 0.08));
                    if let Some(r) = Rect::from_xywh(left, top, right - left, 1.0) {
                        cpm.fill_rect(r, &paint, Transform::identity(), None);
                    }
                }
                let mid = mid_of(top);
                draw_text(&mut cpm, font, desc, px, left, baseline_in(top, key_row_h, px), st.text, 1.0);
                // + (add a key) at the right, the keys to its left
                let b = self.u(26.0);
                let by = (mid - b / 2.0).round();
                let mut x = right - b;
                let r = rounded_rect(x, by, b, b, self.u(4.0));
                fill(&mut cpm, &mut paint, r.clone(), st.text, 0.06);
                stroke(&mut cpm, &mut paint, r, st.text, 0.25, 1.0);
                let (cx, cy, s5) = (x + b / 2.0, by + b / 2.0, self.u(5.0));
                let mut pb = PathBuilder::new();
                pb.move_to(cx - s5, cy);
                pb.line_to(cx + s5, cy);
                pb.move_to(cx, cy - s5);
                pb.line_to(cx, cy + s5);
                stroke(&mut cpm, &mut paint, pb.finish(), st.text, 0.8, self.u(1.4));
                push_hit(&mut hits, Hit::KeyAdd(c), rx + x, by, b, b);
                let keys = self.keymap.keys(*cmd);
                let waiting = |k: Option<usize>| self.capture == Some((c, k));
                let mut chips: Vec<(Option<usize>, String)> =
                    keys.iter().enumerate().map(|(k, spec)| (Some(k), KeyMap::display(spec))).collect();
                if waiting(None) {
                    chips.push((None, String::new()));
                }
                for (k, text) in chips.into_iter().rev() {
                    let active = waiting(k);
                    let text = if active { "Press a key\u{2026}".to_string() } else { text };
                    let remove_w = if k.is_some() && !active { self.u(20.0) } else { 0.0 };
                    let cw = text_width(font, &text, small) + self.u(20.0) + remove_w;
                    x -= cw + self.u(6.0);
                    if x < left + self.u(220.0) {
                        break; // no room left next to the description
                    }
                    let r = rounded_rect(x, by, cw, b, self.u(4.0));
                    if active {
                        fill(&mut cpm, &mut paint, r.clone(), st.selection, 0.35);
                        stroke(&mut cpm, &mut paint, r, st.selection, 0.9, 1.0);
                    } else {
                        fill(&mut cpm, &mut paint, r.clone(), st.text, 0.08);
                        stroke(&mut cpm, &mut paint, r, st.text, 0.3, 1.0);
                    }
                    draw_text(&mut cpm, font, &text, small, x + self.u(10.0), baseline_in(by, b, small), st.text, 1.0);
                    if let Some(k) = k {
                        if remove_w > 0.0 {
                            let (cx, cy, s4) = (x + cw - self.u(14.0), by + b / 2.0, self.u(3.5));
                            let mut pb = PathBuilder::new();
                            pb.move_to(cx - s4, cy - s4);
                            pb.line_to(cx + s4, cy + s4);
                            pb.move_to(cx + s4, cy - s4);
                            pb.line_to(cx - s4, cy + s4);
                            stroke(&mut cpm, &mut paint, pb.finish(), st.text, 0.55, self.u(1.3));
                            push_hit(&mut hits, Hit::KeyRemove(c, k), rx + x + cw - remove_w - self.u(4.0), by, remove_w + self.u(4.0), b);
                        }
                        push_hit(&mut hits, Hit::KeyChip(c, k), rx + x, by, cw - remove_w - self.u(4.0), b);
                    }
                }
            }
            // what cannot be changed
            for (n, (k, d)) in notes.iter().enumerate() {
                let top = self.u(18.0) + key_row_h * (COMMANDS.len() + n) as f32 - self.scroll;
                if top + key_row_h < 0.0 || top > area_h {
                    continue;
                }
                let base = baseline_in(top, key_row_h, px);
                draw_text(&mut cpm, font, d, px, left, base, st.text, 0.55);
                let tw = text_width(font, k, small);
                draw_text(&mut cpm, font, k, small, right - tw - self.u(4.0), base, st.text, 0.55);
            }
        }
        let row_h = self.u(64.0);
        let defs: Vec<(usize, &Def)> = if self.section == KEYS { Vec::new() } else { self.defs().collect() };
        for (n, (i, d)) in defs.into_iter().enumerate() {
            let top = self.u(6.0) + row_h * n as f32 - self.scroll;
            if top + row_h < 0.0 || top > area_h {
                continue; // scrolled out of view
            }
            if n > 0 {
                paint.set_color(color(st.text, 0.08));
                if let Some(r) = Rect::from_xywh(left, top, right - left, 1.0) {
                    cpm.fill_rect(r, &paint, Transform::identity(), None);
                }
            }
            let label = if d.restart { format!("{} (after a restart)", d.label) } else { d.label.to_string() };
            draw_text(&mut cpm, font, &label, px, left, top + self.u(26.0), st.text, 1.0);
            draw_text(&mut cpm, font, d.help, small, left, top + self.u(46.0), st.text, 0.55);
            let mid = top + row_h / 2.0;
            match (&d.kind, self.values.get(i)) {
                (Kind::Bool, Value::Bool(on)) => {
                    // switch
                    let (sw, sh) = (self.u(40.0), self.u(22.0));
                    let (sx, sy) = (right - sw, (mid - sh / 2.0).round());
                    let track = rounded_rect(sx, sy, sw, sh, sh / 2.0);
                    if on {
                        fill(&mut cpm, &mut paint, track, st.selection, 1.0);
                    } else {
                        fill(&mut cpm, &mut paint, track.clone(), st.text, 0.12);
                        stroke(&mut cpm, &mut paint, track, st.text, 0.35, 1.0);
                    }
                    let k = sh - self.u(6.0);
                    let kx = if on { sx + sw - k - self.u(3.0) } else { sx + self.u(3.0) };
                    fill(&mut cpm, &mut paint, rounded_rect(kx, sy + self.u(3.0), k, k, k / 2.0), [255, 255, 255], 1.0);
                    push_hit(&mut hits, Hit::Toggle(i), rx + sx - self.u(8.0), top, sw + self.u(16.0), row_h);
                }
                (Kind::Number { unit, .. }, Value::Number(v)) => {
                    let b = self.u(28.0);
                    let vw = self.u(78.0);
                    let by = (mid - b / 2.0).round();
                    let plus_x = right - b;
                    let minus_x = plus_x - vw - b;
                    for (bx, plus) in [(minus_x, false), (plus_x, true)] {
                        let r = rounded_rect(bx, by, b, b, self.u(4.0));
                        fill(&mut cpm, &mut paint, r.clone(), st.text, 0.08);
                        stroke(&mut cpm, &mut paint, r, st.text, 0.3, 1.0);
                        let (cx, cy, s) = (bx + b / 2.0, by + b / 2.0, self.u(5.0));
                        let mut pb = PathBuilder::new();
                        pb.move_to(cx - s, cy);
                        pb.line_to(cx + s, cy);
                        if plus {
                            pb.move_to(cx, cy - s);
                            pb.line_to(cx, cy + s);
                        }
                        stroke(&mut cpm, &mut paint, pb.finish(), st.text, 0.9, self.u(1.5));
                        push_hit(&mut hits, if plus { Hit::Plus(i) } else { Hit::Minus(i) }, rx + bx, by, b, b);
                    }
                    let text = if unit.is_empty() { format!("{v}") } else { format!("{v} {unit}") };
                    let tw = text_width(font, &text, px);
                    draw_text(&mut cpm, font, &text, px, minus_x + b + (vw - tw) / 2.0, baseline_in(by, b, px), st.text, 1.0);
                }
                (Kind::Choice(options), Value::Choice(c)) => {
                    let text = options.get(c).copied().unwrap_or("");
                    let bw = (text_width(font, text, px) + self.u(48.0)).max(self.u(200.0));
                    let bh = self.u(30.0);
                    let (bx, by) = (right - bw, (mid - bh / 2.0).round());
                    let r = rounded_rect(bx, by, bw, bh, self.u(4.0));
                    fill(&mut cpm, &mut paint, r.clone(), st.text, 0.08);
                    stroke(&mut cpm, &mut paint, r, st.text, 0.3, 1.0);
                    draw_text(&mut cpm, font, text, px, bx + self.u(12.0), baseline_in(by, bh, px), st.text, 1.0);
                    let (ax, ay, s) = (bx + bw - self.u(16.0), by + bh / 2.0, self.u(4.0));
                    let mut pb = PathBuilder::new();
                    pb.move_to(ax - s, ay - s * 0.5);
                    pb.line_to(ax, ay + s * 0.5);
                    pb.line_to(ax + s, ay - s * 0.5);
                    stroke(&mut cpm, &mut paint, pb.finish(), st.text, 0.8, self.u(1.4));
                    push_hit(&mut hits, Hit::Choice(i), rx + bx, by, bw, bh);
                }
                _ => {}
            }
        }
        if self.max_scroll > 0.0 {
            // scroll bar
            let track = area_h - self.u(8.0);
            let bar = (track * area_h / content_h).max(self.u(24.0));
            let y = self.u(4.0) + (track - bar) * self.scroll / self.max_scroll;
            fill(&mut cpm, &mut paint, rounded_rect(w - self.u(10.0), y, self.u(4.0), bar, self.u(2.0)), st.text, 0.3);
        }
        pm.draw_pixmap(0, area_top as i32, cpm.as_ref(), &Default::default(), Transform::identity(), None);
        if let Some(d) = &self.dialog {
            // the question over the dimmed panel; only its buttons take clicks
            paint.set_color(color([0, 0, 0], 0.35));
            if let Some(r) = Rect::from_xywh(0.0, 0.0, w, h) {
                pm.fill_rect(r, &paint, Transform::identity(), None);
            }
            let dw = (w - self.u(48.0)).min(self.u(460.0));
            let pad = self.u(20.0);
            // the text in lines that fit
            let mut lines: Vec<String> = Vec::new();
            for word in d.text.split(' ') {
                match lines.last_mut() {
                    Some(l) if text_width(font, &format!("{l} {word}"), px) <= dw - pad * 2.0 => {
                        l.push(' ');
                        l.push_str(word);
                    }
                    _ => lines.push(word.to_string()),
                }
            }
            let line_h = self.u(24.0);
            let bh = self.u(30.0);
            let dh = pad * 2.0 + line_h * lines.len() as f32 + self.u(14.0) + bh;
            let (dx, dy) = (((w - dw) / 2.0).round(), ((h - dh) / 2.0).round());
            let r = rounded_rect(dx, dy, dw, dh, self.u(8.0));
            fill(&mut pm, &mut paint, r.clone(), st.background, 1.0);
            stroke(&mut pm, &mut paint, r, st.text, 0.25, 1.0);
            for (n, l) in lines.iter().enumerate() {
                draw_text(&mut pm, font, l, px, dx + pad, baseline_in(dy + pad + line_h * n as f32, line_h, px), st.text, 1.0);
            }
            let buttons: Vec<(&str, Hit, bool)> = if d.swap.is_some() {
                vec![
                    ("Cancel", Hit::DialogNo, false),
                    (if d.swap.as_ref().is_some_and(|s| s.2.is_none()) { "Move" } else { "Swap" }, Hit::DialogYes, true),
                ]
            } else {
                vec![("OK", Hit::DialogNo, true)]
            };
            let mut x = dx + dw - pad;
            let by = dy + dh - pad - bh;
            for (label, hit, main) in buttons.into_iter().rev() {
                let bw = (text_width(font, label, px) + self.u(32.0)).max(self.u(84.0));
                x -= bw;
                let r = rounded_rect(x, by, bw, bh, self.u(4.0));
                if main {
                    fill(&mut pm, &mut paint, r.clone(), st.selection, 0.85);
                } else {
                    fill(&mut pm, &mut paint, r.clone(), st.text, 0.08);
                    stroke(&mut pm, &mut paint, r, st.text, 0.3, 1.0);
                }
                let tw = text_width(font, label, px);
                draw_text(
                    &mut pm,
                    font,
                    label,
                    px,
                    x + (bw - tw) / 2.0,
                    baseline_in(by, bh, px),
                    if main { [255, 255, 255] } else { st.text },
                    1.0,
                );
                hits.push((hit, rx + x, ry + by, bw, bh));
                x -= self.u(10.0);
            }
        }
        self.hits = hits;
        Some(pm)
    }
}
