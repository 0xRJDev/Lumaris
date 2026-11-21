// The settings window, drawn inside the Lumaris window: sections on the left, the settings of
// the selected section on the right. Changes apply immediately.
use resvg::tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

use crate::menu::{self, Action, Item, Menu};
use crate::settings::{Def, Kind, Settings, Value, DEFS, SECTIONS};
use crate::ui::{color, draw_text, rounded_rect, style, text_width};

#[derive(Clone, Copy, PartialEq)]
enum Hit {
    Section(usize),
    Close,
    EditFile,
    Toggle(usize),
    Minus(usize),
    Plus(usize),
    Choice(usize),
}

pub enum Outcome {
    Nothing,
    Redraw,
    Changed(Settings),
    Close,
    EditFile,
}

pub struct SettingsPanel {
    values: Settings,
    section: usize,
    scale: f32,
    window: (f32, f32),
    rect: (f32, f32, f32, f32),
    hits: Vec<(Hit, f32, f32, f32, f32)>,
    pixmap: Option<Pixmap>,
    /// open choice list: setting index and its menu
    choice: Option<(usize, Menu)>,
}

impl SettingsPanel {
    pub fn new(values: Settings, window: (u32, u32), scale: f64) -> SettingsPanel {
        let mut p = SettingsPanel {
            values,
            section: 0,
            scale: scale as f32,
            window: (0.0, 0.0),
            rect: (0.0, 0.0, 0.0, 0.0),
            hits: Vec::new(),
            pixmap: None,
            choice: None,
        };
        p.resize(window);
        p
    }

    fn u(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }

    pub fn resize(&mut self, window: (u32, u32)) {
        self.window = (window.0 as f32, window.1 as f32);
        let (ww, wh) = self.window;
        let w = (ww - self.u(24.0)).min(self.u(860.0)).max(1.0).floor();
        let h = (wh - self.u(24.0)).min(self.u(560.0)).max(1.0).floor();
        self.rect = (((ww - w) / 2.0).round(), ((wh - h) / 2.0).round(), w, h);
        self.pixmap = None;
        self.choice = None;
    }

    /// Size the window should have at least to show the panel (window pixels)
    pub fn wanted_size(scale: f64) -> (u32, u32) {
        ((884.0 * scale) as u32, (584.0 * scale) as u32)
    }

    fn defs(&self) -> impl Iterator<Item = (usize, &'static Def)> {
        let section = SECTIONS[self.section];
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
        let (rx, ry, rw, rh) = self.rect;
        let (x, y) = (p.0 as f32, p.1 as f32);
        if x < rx || y < ry || x >= rx + rw || y >= ry + rh {
            return Outcome::Close;
        }
        match self.hit(p) {
            Some(Hit::Section(s)) => {
                self.section = s;
                self.pixmap = None;
                Outcome::Redraw
            }
            Some(Hit::Close) => Outcome::Close,
            Some(Hit::EditFile) => Outcome::EditFile,
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
                    let m = Menu::open(items, (hx as f64, (hy + hh) as f64), (self.window.0 as u32, self.window.1 as u32), self.scale as f64);
                    self.choice = Some((i, m));
                }
                Outcome::Redraw
            }
            None => Outcome::Nothing,
        }
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
        for (i, name) in SECTIONS.iter().enumerate() {
            let top = self.u(64.0) + item_h * i as f32;
            if i == self.section {
                let r = rounded_rect(self.u(8.0), top, side - self.u(16.0), item_h, self.u(4.0));
                fill(&mut pm, &mut paint, r.clone(), st.selection, 0.35);
                stroke(&mut pm, &mut paint, r, st.selection, 0.9, 1.0);
            }
            draw_text(&mut pm, font, name, px, self.u(20.0), baseline_in(top, item_h, px), st.text, 1.0);
            hits.push((Hit::Section(i), rx + self.u(8.0), ry + top, side - self.u(16.0), item_h));
        }
        // edit the file
        let link = "Edit settings file…";
        let link_top = h - self.u(48.0);
        draw_text(&mut pm, font, link, small, self.u(20.0), baseline_in(link_top, self.u(32.0), small), st.selection, 1.0);
        hits.push((Hit::EditFile, rx + self.u(12.0), ry + link_top, text_width(font, link, small) + self.u(16.0), self.u(32.0)));

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
        draw_text(&mut pm, font, SECTIONS[self.section], title, left, baseline_in(self.u(14.0), self.u(40.0), title), st.text, 1.0);
        let row_h = self.u(64.0);
        let defs: Vec<(usize, &Def)> = self.defs().collect();
        for (n, (i, d)) in defs.into_iter().enumerate() {
            let top = self.u(70.0) + row_h * n as f32;
            if n > 0 {
                paint.set_color(color(st.text, 0.08));
                if let Some(r) = Rect::from_xywh(left, top, right - left, 1.0) {
                    pm.fill_rect(r, &paint, Transform::identity(), None);
                }
            }
            let label = if d.restart { format!("{} (after a restart)", d.label) } else { d.label.to_string() };
            draw_text(&mut pm, font, &label, px, left, top + self.u(26.0), st.text, 1.0);
            draw_text(&mut pm, font, d.help, small, left, top + self.u(46.0), st.text, 0.55);
            let mid = top + row_h / 2.0;
            match (&d.kind, self.values.get(i)) {
                (Kind::Bool, Value::Bool(on)) => {
                    // switch
                    let (sw, sh) = (self.u(40.0), self.u(22.0));
                    let (sx, sy) = (right - sw, (mid - sh / 2.0).round());
                    let track = rounded_rect(sx, sy, sw, sh, sh / 2.0);
                    if on {
                        fill(&mut pm, &mut paint, track, st.selection, 1.0);
                    } else {
                        fill(&mut pm, &mut paint, track.clone(), st.text, 0.12);
                        stroke(&mut pm, &mut paint, track, st.text, 0.35, 1.0);
                    }
                    let k = sh - self.u(6.0);
                    let kx = if on { sx + sw - k - self.u(3.0) } else { sx + self.u(3.0) };
                    fill(&mut pm, &mut paint, rounded_rect(kx, sy + self.u(3.0), k, k, k / 2.0), [255, 255, 255], 1.0);
                    hits.push((Hit::Toggle(i), rx + sx - self.u(8.0), ry + top, sw + self.u(16.0), row_h));
                }
                (Kind::Number { unit, .. }, Value::Number(v)) => {
                    let b = self.u(28.0);
                    let vw = self.u(78.0);
                    let by = (mid - b / 2.0).round();
                    let plus_x = right - b;
                    let minus_x = plus_x - vw - b;
                    for (bx, plus) in [(minus_x, false), (plus_x, true)] {
                        let r = rounded_rect(bx, by, b, b, self.u(4.0));
                        fill(&mut pm, &mut paint, r.clone(), st.text, 0.08);
                        stroke(&mut pm, &mut paint, r, st.text, 0.3, 1.0);
                        let (cx, cy, s) = (bx + b / 2.0, by + b / 2.0, self.u(5.0));
                        let mut pb = PathBuilder::new();
                        pb.move_to(cx - s, cy);
                        pb.line_to(cx + s, cy);
                        if plus {
                            pb.move_to(cx, cy - s);
                            pb.line_to(cx, cy + s);
                        }
                        stroke(&mut pm, &mut paint, pb.finish(), st.text, 0.9, self.u(1.5));
                        hits.push((if plus { Hit::Plus(i) } else { Hit::Minus(i) }, rx + bx, ry + by, b, b));
                    }
                    let text = if unit.is_empty() { format!("{v}") } else { format!("{v} {unit}") };
                    let tw = text_width(font, &text, px);
                    draw_text(&mut pm, font, &text, px, minus_x + b + (vw - tw) / 2.0, baseline_in(by, b, px), st.text, 1.0);
                }
                (Kind::Choice(options), Value::Choice(c)) => {
                    let text = options.get(c).copied().unwrap_or("");
                    let bw = (text_width(font, text, px) + self.u(48.0)).max(self.u(200.0));
                    let bh = self.u(30.0);
                    let (bx, by) = (right - bw, (mid - bh / 2.0).round());
                    let r = rounded_rect(bx, by, bw, bh, self.u(4.0));
                    fill(&mut pm, &mut paint, r.clone(), st.text, 0.08);
                    stroke(&mut pm, &mut paint, r, st.text, 0.3, 1.0);
                    draw_text(&mut pm, font, text, px, bx + self.u(12.0), baseline_in(by, bh, px), st.text, 1.0);
                    let (ax, ay, s) = (bx + bw - self.u(16.0), by + bh / 2.0, self.u(4.0));
                    let mut pb = PathBuilder::new();
                    pb.move_to(ax - s, ay - s * 0.5);
                    pb.line_to(ax, ay + s * 0.5);
                    pb.line_to(ax + s, ay - s * 0.5);
                    stroke(&mut pm, &mut paint, pb.finish(), st.text, 0.8, self.u(1.4));
                    hits.push((Hit::Choice(i), rx + bx, ry + by, bw, bh));
                }
                _ => {}
            }
        }
        self.hits = hits;
        Some(pm)
    }
}
