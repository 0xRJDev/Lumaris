// A short message in the window (the zoom while zooming): shown for a moment, then hidden.
use std::time::{Duration, Instant};

use resvg::tiny_skia::{FillRule, Paint, Pixmap, Transform};

use crate::ui::{color, draw_text, rounded_rect, style, text_width};

const SHOW_FOR: Duration = Duration::from_millis(900);

pub struct Toast {
    text: String,
    until: Option<Instant>,
    pixmap: Option<Pixmap>,
    scale: f32,
}

impl Toast {
    pub fn new() -> Toast {
        Toast { text: String::new(), until: None, pixmap: None, scale: 1.0 }
    }

    /// Shows the text (again) for a moment. Returns true when the picture changed.
    pub fn show(&mut self, text: String, scale: f64) -> bool {
        self.until = Some(Instant::now() + SHOW_FOR);
        if text != self.text || self.scale != scale as f32 {
            self.text = text;
            self.scale = scale as f32;
            self.pixmap = None;
            return true;
        }
        false
    }

    /// Shows a message for longer than the zoom (time to read it).
    pub fn message(&mut self, text: String, scale: f64) {
        self.show(text, scale);
        self.until = Some(Instant::now() + SHOW_FOR * 3);
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.until
    }

    /// Called at the deadline. Returns true when the toast was hidden.
    pub fn expire(&mut self) -> bool {
        if self.until.is_some_and(|t| Instant::now() >= t) {
            self.until = None;
            return true;
        }
        false
    }

    pub fn layer(&mut self, window: (u32, u32)) -> Option<(&[u8], u32, u32, i32, i32)> {
        self.until?;
        if self.pixmap.is_none() {
            self.pixmap = self.paint();
        }
        let p = self.pixmap.as_ref()?;
        let x = (window.0 as i32 - p.width() as i32) / 2;
        let y = window.1 as i32 - p.height() as i32 - (90.0 * self.scale) as i32;
        Some((p.data(), p.width(), p.height(), x, y.max(0)))
    }

    fn paint(&self) -> Option<Pixmap> {
        let st = style();
        let font = st.font.as_ref()?;
        let px = (st.point_size * 96.0 / 72.0 * 1.3 * self.scale).round();
        let pad = (12.0 * self.scale).round();
        let w = (text_width(font, &self.text, px) + pad * 2.0).ceil();
        let h = (px * 1.9).ceil();
        let mut pm = Pixmap::new(w as u32, h as u32)?;
        let mut paint = Paint { anti_alias: true, ..Default::default() };
        if let Some(r) = rounded_rect(0.0, 0.0, w, h, h / 2.0) {
            paint.set_color(color(st.background, 0.85));
            pm.fill_path(&r, &paint, FillRule::Winding, Transform::identity(), None);
        }
        let (a, d) = font.horizontal_line_metrics(px).map(|m| (m.ascent, m.descent)).unwrap_or((px * 0.8, -px * 0.2));
        let baseline = ((h - (a - d)) / 2.0 + a).round();
        draw_text(&mut pm, font, &self.text, px, pad, baseline, st.text, 1.0);
        Some(pm)
    }
}
