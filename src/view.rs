// Zoom and pan state: where the image is drawn in the window.
//
// The image position is stored as an anchor point in the window plus the image point under it
// (relative to the image centre, in image pixels). While the zoom glides towards its target the
// anchor stays fixed, so the point under the mouse does not move. The edges are clamped so the
// image never leaves gaps it could fill.
use std::time::Instant;

const MIN_ZOOM: f64 = 0.02;
const MAX_ZOOM: f64 = 64.0;
const GLIDE_SECONDS: f64 = 0.045; // time constant of the smooth zoom

pub struct View {
    img: (f64, f64),
    win: (f64, f64),
    pub zoom: f64,
    target: f64,
    anchor: (f64, f64),
    point: (f64, f64),
    last_tick: Option<Instant>,
    fit_mode: bool,
    /// fit also enlarges small images
    enlarge: bool,
    smooth: bool,
    /// fit enlarges small images at least to this zoom (smallest opening size)
    min_fit: f64,
}

impl View {
    pub fn new() -> View {
        View { img: (1.0, 1.0), win: (1.0, 1.0), zoom: 1.0, target: 1.0, anchor: (0.0, 0.0), point: (0.0, 0.0), last_tick: None, fit_mode: true, enlarge: false, smooth: true, min_fit: 1.0 }
    }

    pub fn configure(&mut self, enlarge: bool, smooth: bool) {
        self.enlarge = enlarge;
        self.smooth = smooth;
    }

    /// Zoom that shows the whole image (enlarges small images only if configured).
    pub fn fit_zoom(&self) -> f64 {
        let z = (self.win.0 / self.img.0).min(self.win.1 / self.img.1);
        if self.enlarge { z } else { z.min(1.0).max(self.min_fit.min(z)) }
    }

    /// The zoom being glided to
    pub fn target_zoom(&self) -> f64 {
        self.target
    }

    pub fn set_min_fit_zoom(&mut self, z: f64) {
        self.min_fit = z;
    }

    /// Another image at the same zoom, centred (keep zoom on navigation).
    pub fn replace_image(&mut self, w: u32, h: u32) {
        self.img = (w.max(1) as f64, h.max(1) as f64);
        self.anchor = (self.win.0 / 2.0, self.win.1 / 2.0);
        self.point = (0.0, 0.0);
        self.target = self.zoom;
        self.last_tick = None;
    }

    pub fn set_image(&mut self, w: u32, h: u32) {
        self.img = (w.max(1) as f64, h.max(1) as f64);
        self.fit();
    }

    pub fn fit(&mut self) {
        self.fit_mode = true;
        self.zoom = self.fit_zoom();
        self.target = self.zoom;
        self.anchor = (self.win.0 / 2.0, self.win.1 / 2.0);
        self.point = (0.0, 0.0);
        self.last_tick = None;
    }

    /// The window was resized around its centre (KWin keeps the centre in place): the content
    /// keeps its position on the screen, also during a zoom animation.
    pub fn resize(&mut self, w: u32, h: u32) {
        let old = self.win;
        self.win = (w.max(1) as f64, h.max(1) as f64);
        if self.fit_mode {
            self.fit();
        } else {
            self.anchor.0 += (self.win.0 - old.0) / 2.0;
            self.anchor.1 += (self.win.1 - old.1) / 2.0;
        }
    }

    /// true while the image is fitted to the window (not zoomed by the user)
    pub fn is_fit(&self) -> bool {
        self.fit_mode
    }

    pub fn image_size(&self) -> (f64, f64) {
        self.img
    }

    /// Zooms by `factor` around `at` (window pixels), smoothly.
    pub fn zoom_at(&mut self, factor: f64, at: (f64, f64)) {
        let c = self.center_at(self.zoom);
        self.anchor = at;
        self.point = ((at.0 - c.0) / self.zoom, (at.1 - c.1) / self.zoom);
        self.target = (self.target * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.fit_mode = false;
        if !self.smooth {
            self.zoom = self.target;
            let c = self.center_at(self.zoom);
            self.set_center(c);
            self.last_tick = None;
        } else if self.last_tick.is_none() {
            self.last_tick = Some(Instant::now());
        }
    }

    /// Glides to `zoom` around the window centre.
    pub fn zoom_to(&mut self, zoom: f64) {
        let at = (self.win.0 / 2.0, self.win.1 / 2.0);
        self.zoom_at(zoom / self.target, at);
    }

    /// Shows the image point at fractions (fx, fy) in the window centre (zoom map)
    pub fn center_on(&mut self, fx: f64, fy: f64) {
        let (w, h) = (self.img.0 * self.zoom, self.img.1 * self.zoom);
        let c = (self.win.0 / 2.0 + (0.5 - fx) * w, self.win.1 / 2.0 + (0.5 - fy) * h);
        self.set_center(c);
        self.fit_mode = false;
    }

    pub fn pan(&mut self, dx: f64, dy: f64) {
        let c = self.center_at(self.zoom);
        self.set_center((c.0 + dx, c.1 + dy));
        self.fit_mode = false;
    }

    fn set_center(&mut self, c: (f64, f64)) {
        let c = self.clamp(c, self.zoom);
        self.point = ((self.anchor.0 - c.0) / self.zoom, (self.anchor.1 - c.1) / self.zoom);
    }

    /// Advances the zoom animation. Returns true while it is still running.
    pub fn step(&mut self, now: Instant) -> bool {
        let Some(last) = self.last_tick else { return false };
        let dt = (now - last).as_secs_f64().min(0.05);
        self.last_tick = Some(now);
        let k = 1.0 - (-dt / GLIDE_SECONDS).exp();
        let lz = self.zoom.ln() + (self.target.ln() - self.zoom.ln()) * k;
        self.zoom = lz.exp();
        if (self.target / self.zoom).ln().abs() < 1e-3 {
            // finish exactly at the target and store the clamped position
            self.zoom = self.target;
            let c = self.center_at(self.zoom);
            self.set_center(c);
            self.last_tick = None;
            return false;
        }
        true
    }

    /// Image centre in window pixels at zoom `z` (clamped).
    fn center_at(&self, z: f64) -> (f64, f64) {
        self.clamp((self.anchor.0 - self.point.0 * z, self.anchor.1 - self.point.1 * z), z)
    }

    fn clamp(&self, c: (f64, f64), z: f64) -> (f64, f64) {
        let axis = |c: f64, img: f64, win: f64| {
            let e = img * z;
            if e <= win { win / 2.0 } else { c.clamp(win - e / 2.0, e / 2.0) }
        };
        (axis(c.0, self.img.0, self.win.0), axis(c.1, self.img.1, self.win.1))
    }

    /// Image rectangle in window pixels: [x0, y0, x1, y1].
    pub fn image_rect(&self) -> [f32; 4] {
        let c = self.center_at(self.zoom);
        let (w, h) = (self.img.0 * self.zoom, self.img.1 * self.zoom);
        let (mut x0, mut y0) = (c.0 - w / 2.0, c.1 - h / 2.0);
        if (self.zoom - 1.0).abs() < 1e-9 {
            // 1:1 - align to pixels so the image stays exactly sharp
            x0 = x0.round();
            y0 = y0.round();
        }
        [x0 as f32, y0 as f32, (x0 + w) as f32, (y0 + h) as f32]
    }
}
