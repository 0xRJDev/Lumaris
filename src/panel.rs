// Navigation panel: a button bar at the bottom of the window (shown while the mouse moves)
// and, when the image is larger than the window, a zoom map in the corner that shows the
// visible part (click or drag on it to move there).
use std::time::{Duration, Instant};

use resvg::tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::keys::Command;
use crate::ui::{color, rounded_rect, style};

const HIDE_AFTER: Duration = Duration::from_millis(1800);
const BUTTONS: &[Command] = &[
    Command::Previous,
    Command::Next,
    Command::RotateLeft,
    Command::RotateRight,
    Command::ToggleFit,
    Command::FullScreen,
    Command::ImageInfo,
];

pub struct Panel {
    scale: f32,
    window: (u32, u32),
    visible_until: Option<Instant>,
    hover: Option<usize>,
    bar: Option<Pixmap>,
    /// small copy of the image (premultiplied RGBA) for the zoom map
    thumb: Option<Pixmap>,
    map: Option<Pixmap>,
    /// visible part of the image (fractions x0, y0, x1, y1), None when all is visible
    view: Option<[f32; 4]>,
}

impl Panel {
    pub fn new() -> Panel {
        Panel { scale: 1.0, window: (1, 1), visible_until: None, hover: None, bar: None, thumb: None, map: None, view: None }
    }

    fn u(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }

    pub fn configure(&mut self, window: (u32, u32), scale: f64) {
        if self.window != window || self.scale != scale as f32 {
            self.window = window;
            self.scale = scale as f32;
            self.bar = None;
            self.map = None;
        }
    }

    /// The mouse moved: show the bar for a while. Returns true when it must be redrawn.
    pub fn poke(&mut self, at: (f64, f64)) -> bool {
        let was = self.visible();
        self.visible_until = Some(Instant::now() + HIDE_AFTER);
        let hover = self.button_at(at);
        let changed = hover != self.hover;
        if changed {
            self.hover = hover;
            self.bar = None;
        }
        !was || changed
    }

    pub fn visible(&self) -> bool {
        self.visible_until.is_some_and(|t| Instant::now() < t || self.hover.is_some())
    }

    /// When the bar hides (for the event loop timer)
    pub fn deadline(&self) -> Option<Instant> {
        self.visible_until.filter(|_| self.hover.is_none())
    }

    /// Called at the deadline. Returns true when the bar was hidden (redraw).
    pub fn expire(&mut self) -> bool {
        if self.visible_until.is_some_and(|t| Instant::now() >= t) && self.hover.is_none() {
            self.visible_until = None;
            return true;
        }
        false
    }

    fn bar_rect(&self) -> (f32, f32, f32, f32) {
        let b = self.u(40.0);
        let gap = self.u(4.0);
        let w = BUTTONS.len() as f32 * (b + gap) + gap + self.u(16.0);
        let h = b + gap * 2.0;
        let x = ((self.window.0 as f32 - w) / 2.0).round();
        let y = self.window.1 as f32 - h - self.u(16.0);
        (x, y, w, h)
    }

    fn button_rect(&self, i: usize) -> (f32, f32, f32, f32) {
        let (x, y, _, _) = self.bar_rect();
        let b = self.u(40.0);
        let gap = self.u(4.0);
        let extra = if i >= 2 { self.u(8.0) } else { 0.0 } + if i >= 4 { self.u(8.0) } else { 0.0 };
        (x + gap + i as f32 * (b + gap) + extra, y + gap, b, b)
    }

    fn large_enough(&self) -> bool {
        self.window.0 as f32 >= self.u(380.0) && self.window.1 as f32 >= self.u(200.0)
    }

    fn button_at(&self, p: (f64, f64)) -> Option<usize> {
        if !self.large_enough() {
            return None;
        }
        let (x, y) = (p.0 as f32, p.1 as f32);
        (0..BUTTONS.len()).find(|&i| {
            let (bx, by, bw, bh) = self.button_rect(i);
            x >= bx && x < bx + bw && y >= by && y < by + bh
        })
    }

    /// A click: the command of a button, or a position in the zoom map (fractions of the image)
    pub fn click(&self, p: (f64, f64)) -> Option<Result<Command, (f64, f64)>> {
        if self.visible()
            && let Some(i) = self.button_at(p)
        {
            return Some(Ok(BUTTONS[i]));
        }
        self.map_position(p).map(Err)
    }

    /// Position in the zoom map as fractions of the image
    pub fn map_position(&self, p: (f64, f64)) -> Option<(f64, f64)> {
        let (mx, my, mw, mh) = self.map_rect()?;
        let (x, y) = (p.0 as f32, p.1 as f32);
        (x >= mx && x < mx + mw && y >= my && y < my + mh).then(|| (((x - mx) / mw) as f64, ((y - my) / mh) as f64))
    }

    fn map_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let t = self.thumb.as_ref()?;
        self.view?;
        if !self.large_enough() {
            return None;
        }
        let (w, h) = (t.width() as f32, t.height() as f32);
        let m = self.u(16.0);
        Some((
            self.window.0 as f32 - w - m,
            self.window.1 as f32 - h - m - if self.visible() { self.bar_rect().3 + self.u(8.0) } else { 0.0 },
            w,
            h,
        ))
    }

    /// New image (straight alpha RGBA) for the zoom map. `orient` as in the renderer.
    pub fn set_image(&mut self, rgba: &[u8], w: u32, h: u32, orient: u32) {
        self.thumb = None;
        self.map = None;
        let Some(img) = image::RgbaImage::from_raw(w, h, rgba.to_vec()) else { return };
        let max = self.u(170.0) as u32;
        let s = (max as f64 / w.max(h) as f64).min(1.0);
        let mut t = image::imageops::thumbnail(&img, ((w as f64 * s) as u32).max(1), ((h as f64 * s) as u32).max(1));
        if orient & 4 != 0 {
            t = image::imageops::flip_horizontal(&t);
        }
        t = match orient & 3 {
            1 => image::imageops::rotate90(&t),
            2 => image::imageops::rotate180(&t),
            3 => image::imageops::rotate270(&t),
            _ => t,
        };
        let (tw, th) = t.dimensions();
        let Some(mut pm) = Pixmap::new(tw, th) else { return };
        for (d, s) in pm.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(t.pixels()) {
            let a = s.0[3] as u32;
            d[0] = (s.0[0] as u32 * a / 255) as u8;
            d[1] = (s.0[1] as u32 * a / 255) as u8;
            d[2] = (s.0[2] as u32 * a / 255) as u8;
            d[3] = a as u8;
        }
        self.thumb = Some(pm);
    }

    /// The zoom map is needed but has no picture yet
    pub fn wants_image(&self) -> bool {
        self.view.is_some() && self.thumb.is_none()
    }

    pub fn clear_image(&mut self) {
        self.thumb = None;
        self.map = None;
    }

    /// The image rectangle in window pixels; the zoom map shows when it is larger than the window.
    pub fn set_view(&mut self, rect: [f32; 4]) -> bool {
        let (ww, wh) = (self.window.0 as f32, self.window.1 as f32);
        let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
        let v = if w > ww + 1.0 || h > wh + 1.0 {
            Some([
                (-rect[0] / w).clamp(0.0, 1.0),
                (-rect[1] / h).clamp(0.0, 1.0),
                ((ww - rect[0]) / w).clamp(0.0, 1.0),
                ((wh - rect[1]) / h).clamp(0.0, 1.0),
            ])
        } else {
            None
        };
        if v != self.view {
            self.view = v;
            self.map = None;
            return true;
        }
        false
    }

    pub fn layers(&mut self) -> Vec<(&[u8], u32, u32, i32, i32)> {
        let mut out = Vec::new();
        if !self.large_enough() {
            return out;
        }
        if self.visible() && self.bar.is_none() {
            self.bar = self.paint_bar();
        }
        if self.view.is_some() && self.map.is_none() {
            self.map = self.paint_map();
        }
        let (bx, by, _, _) = self.bar_rect();
        let map_pos = self.map_rect();
        if self.visible()
            && let Some(b) = &self.bar
        {
            out.push((b.data(), b.width(), b.height(), bx as i32, by as i32));
        }
        if let (Some(m), Some((mx, my, _, _))) = (&self.map, map_pos) {
            let pad = self.u(4.0);
            out.push((m.data(), m.width(), m.height(), (mx - pad) as i32, (my - pad) as i32));
        }
        out
    }

    fn paint_map(&self) -> Option<Pixmap> {
        let st = style();
        let t = self.thumb.as_ref()?;
        let v = self.view?;
        let pad = self.u(4.0);
        let (w, h) = (t.width() as f32 + pad * 2.0, t.height() as f32 + pad * 2.0);
        let mut pm = Pixmap::new(w as u32, h as u32)?;
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        if let Some(r) = rounded_rect(0.5, 0.5, w - 1.0, h - 1.0, self.u(5.0)) {
            paint.set_color(color(st.background, 0.85));
            pm.fill_path(&r, &paint, FillRule::Winding, Transform::identity(), None);
        }
        pm.draw_pixmap(pad as i32, pad as i32, t.as_ref(), &Default::default(), Transform::identity(), None);
        let (tw, th) = (t.width() as f32, t.height() as f32);
        let r = rounded_rect(pad + v[0] * tw, pad + v[1] * th, ((v[2] - v[0]) * tw).max(3.0), ((v[3] - v[1]) * th).max(3.0), 2.0);
        if let Some(r) = r {
            paint.set_color(color([255, 255, 255], 0.15));
            pm.fill_path(&r, &paint, FillRule::Winding, Transform::identity(), None);
            paint.set_color(color(st.selection, 1.0));
            pm.stroke_path(&r, &paint, &Stroke { width: self.u(1.5), ..Default::default() }, Transform::identity(), None);
        }
        Some(pm)
    }

    fn paint_bar(&self) -> Option<Pixmap> {
        let st = style();
        let (bx, by, w, h) = self.bar_rect();
        let mut pm = Pixmap::new(w as u32, h as u32)?;
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        if let Some(r) = rounded_rect(0.5, 0.5, w - 1.0, h - 1.0, self.u(8.0)) {
            paint.set_color(color(st.background, 0.85));
            pm.fill_path(&r, &paint, FillRule::Winding, Transform::identity(), None);
            paint.set_color(color(st.text, 0.15));
            pm.stroke_path(&r, &paint, &Stroke { width: 1.0, ..Default::default() }, Transform::identity(), None);
        }
        for (i, cmd) in BUTTONS.iter().enumerate() {
            let (x, y, s, _) = self.button_rect(i);
            let (x, y) = (x - bx, y - by);
            if self.hover == Some(i)
                && let Some(r) = rounded_rect(x, y, s, s, self.u(5.0))
            {
                paint.set_color(color(st.selection, 0.35));
                pm.fill_path(&r, &paint, FillRule::Winding, Transform::identity(), None);
            }
            self.icon(&mut pm, *cmd, x + s / 2.0, y + s / 2.0, st.text);
        }
        Some(pm)
    }

    fn icon(&self, pm: &mut Pixmap, cmd: Command, cx: f32, cy: f32, rgb: [u8; 3]) {
        let s = self.u(8.0);
        let mut pb = PathBuilder::new();
        let mut fill = false;
        match cmd {
            Command::Previous => {
                pb.move_to(cx + s * 0.5, cy - s);
                pb.line_to(cx - s * 0.6, cy);
                pb.line_to(cx + s * 0.5, cy + s);
                pb.close();
                fill = true;
            }
            Command::Next => {
                pb.move_to(cx - s * 0.5, cy - s);
                pb.line_to(cx + s * 0.6, cy);
                pb.line_to(cx - s * 0.5, cy + s);
                pb.close();
                fill = true;
            }
            Command::RotateLeft | Command::RotateRight => {
                // three quarters of a circle with an arrow head
                let r = s * 0.85;
                let dir = if cmd == Command::RotateRight { 1.0 } else { -1.0 };
                let steps = 20;
                for k in 0..=steps {
                    let a = std::f32::consts::PI * (-0.5 + dir * 1.5 * k as f32 / steps as f32);
                    let (px, py) = (cx + r * a.cos(), cy + r * a.sin());
                    if k == 0 { pb.move_to(px, py) } else { pb.line_to(px, py) }
                }
                pb.move_to(cx - dir * s * 0.5, cy - r - s * 0.45);
                pb.line_to(cx, cy - r);
                pb.line_to(cx - dir * s * 0.5, cy - r + s * 0.45);
            }
            Command::ToggleFit => {
                // a frame with arrows to the corners
                pb.push_rect(resvg::tiny_skia::Rect::from_xywh(cx - s, cy - s * 0.75, s * 2.0, s * 1.5).unwrap());
                pb.move_to(cx - s * 0.4, cy);
                pb.line_to(cx + s * 0.4, cy);
            }
            Command::FullScreen => {
                for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                    pb.move_to(cx + dx * s, cy + dy * s * 0.35);
                    pb.line_to(cx + dx * s, cy + dy * s);
                    pb.line_to(cx + dx * s * 0.35, cy + dy * s);
                }
            }
            Command::ImageInfo => {
                let r = s * 0.95;
                pb.push_circle(cx, cy, r);
                pb.move_to(cx, cy - s * 0.1);
                pb.line_to(cx, cy + s * 0.5);
                pb.push_circle(cx, cy - s * 0.42, s * 0.06);
            }
            _ => {}
        }
        let Some(path) = pb.finish() else { return };
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        paint.set_color(color(rgb, 0.9));
        if fill {
            pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        } else {
            let stroke = Stroke { width: self.u(1.6), line_cap: resvg::tiny_skia::LineCap::Round, ..Default::default() };
            pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }
}
