// Window size: the size a new image opens with, following the zoom, screen limits.
use crate::*;

impl App {
    /// Zoom an image opens with: fitted into the opening area, small images enlarged to the
    /// smallest opening size.
    pub(crate) fn open_zoom(&self, w: u32, h: u32, screen: PhysicalSize<u32>) -> f64 {
        let (iw, ih) = (w.max(1) as f64, h.max(1) as f64);
        let s = settings::current();
        let percent = s.number("MaxWindowSize");
        let mut aw = screen.width as f64 * percent / 100.0;
        let ah = screen.height as f64 * percent / 100.0;
        aw = aw.min(ah * max_aspect().max(iw / ih));
        let fit = (aw / iw).min(ah / ih);
        let smallest = s.number("MinWindowSize") / 100.0 * screen.height as f64 / iw.max(ih);
        fit.min(1.0).max(smallest.min(fit))
    }

    pub(crate) fn window_size_for(&self, w: u32, h: u32, screen: PhysicalSize<u32>) -> PhysicalSize<u32> {
        let (iw, ih) = (w.max(1) as f64, h.max(1) as f64);
        let z = self.open_zoom(w, h, screen);
        PhysicalSize::new(((iw * z).round() as u32).max(MIN_WINDOW.0), ((ih * z).round() as u32).max(MIN_WINDOW.1))
    }

    /// Largest window content size that keeps the window (with its frame) on the screen.
    pub(crate) fn max_client_size(&self, event_loop: &ActiveEventLoop) -> (f64, f64) {
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
        if let Some(f) = self.kwin.as_ref().and_then(|k| k.frame()) {
            let w = (f.area.0 - f.margins[0] - f.margins[2]) as f64 * scale;
            let h = (f.area.1 - f.margins[1] - f.margins[3]) as f64 * scale;
            if w > 0.0 && h > 0.0 {
                return (w, h);
            }
        }
        let s = self.screen_size(event_loop); // not on KWin: leave room for panels and title bar
        (s.width as f64 * 0.95, s.height as f64 * 0.9)
    }

    pub(crate) fn windowed(&self) -> Option<Arc<Window>> {
        self.window.clone().filter(|w| w.fullscreen().is_none() && !w.is_maximized())
    }

    pub(crate) fn apply_window_size(&mut self, size: PhysicalSize<u32>) {
        let Some(window) = self.window.clone() else { return };
        if size == window.inner_size() {
            return;
        }
        if let Some(now) = window.request_inner_size(size) {
            if let Some(r) = self.renderer.as_mut() {
                r.resize(now.width, now.height);
            }
            self.view.resize(now.width, now.height);
        }
    }

    /// Fits the window to the image (windowed mode) and the image to the window.
    pub(crate) fn fit(&mut self, event_loop: &ActiveEventLoop) {
        let (iw, ih) = self.view.image_size();
        let open = self.open_zoom(iw as u32, ih as u32, self.screen_size(event_loop));
        self.view.set_min_fit_zoom(open.max(1.0)); // small images: enlarged at least to the opening size
        if self.windowed().is_some() {
            let size = self.window_size_for(iw as u32, ih as u32, self.screen_size(event_loop));
            self.apply_window_size(size);
        }
        self.view.fit();
        self.followed_zoom = self.view.zoom;
        self.zoom_in_place = false;
        self.redraw();
    }

    /// While zooming the window grows and shrinks with the image, up to the screen size.
    /// KWin keeps the window centre in place (see kwin.rs).
    pub(crate) fn follow_zoom(&mut self, event_loop: &ActiveEventLoop) {
        let z = self.view.zoom;
        if self.view.is_fit() || (z - self.followed_zoom).abs() < 1e-9 {
            return;
        }
        self.followed_zoom = z;
        if self.zoom_in_place {
            return; // Shift+wheel
        }
        if self.windowed().is_none() || !settings::current().bool("WindowFollowsZoom") {
            return;
        }
        let (iw, ih) = self.view.image_size();
        let (mut mw, mh) = self.max_client_size(event_loop);
        mw = mw.min(mh * max_aspect().max(iw / ih));
        let w = ((iw * z).round().min(mw.floor()) as u32).max(MIN_WINDOW.0);
        let h = ((ih * z).round().min(mh.floor()) as u32).max(MIN_WINDOW.1);
        self.apply_window_size(PhysicalSize::new(w, h));
    }

    pub(crate) fn screen_size(&self, event_loop: &ActiveEventLoop) -> PhysicalSize<u32> {
        self.window
            .as_ref()
            .and_then(|w| w.current_monitor())
            .or_else(|| event_loop.primary_monitor())
            .or_else(|| event_loop.available_monitors().next())
            .map(|m| m.size())
            .unwrap_or(PhysicalSize::new(1920, 1080))
    }

    /// Where panels may be shown beyond the window (popup surfaces): the usable screen area in
    /// window coordinates, a few pixels inside its edges. None: inside the window only.
    pub(crate) fn screen_bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let window = self.window.as_ref()?;
        self.popups.as_ref()?;
        if window.fullscreen().is_some() {
            return None;
        }
        let f = self.kwin.as_ref()?.frame()?;
        let (scale, e) = (window.scale_factor(), 4.0); // KWin centres layer surfaces that touch the edges
        Some((
            ((-f.client.0 as f64 + e) * scale) as f32,
            ((-f.client.1 as f64 + e) * scale) as f32,
            (((f.area.0 - f.client.0) as f64 - e) * scale) as f32,
            (((f.area.1 - f.client.1) as f64 - e) * scale) as f32,
        ))
    }

    pub(crate) fn toggle_fullscreen(&self) {
        if let Some(w) = &self.window {
            w.set_fullscreen(if w.fullscreen().is_some() { None } else { Some(Fullscreen::Borderless(None)) });
        }
    }

    pub(crate) fn window_center(&self) -> (f64, f64) {
        self.window.as_ref().map(|w| w.inner_size()).map(|s| (s.width as f64 / 2.0, s.height as f64 / 2.0)).unwrap_or((0.0, 0.0))
    }

    /// Largest window content size (without an event loop at hand)
    pub(crate) fn max_client_size_now(&self) -> (f64, f64) {
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
        if let Some(f) = self.kwin.as_ref().and_then(|k| k.frame()) {
            let w = (f.area.0 - f.margins[0] - f.margins[2]) as f64 * scale;
            let h = (f.area.1 - f.margins[1] - f.margins[3]) as f64 * scale;
            if w > 0.0 && h > 0.0 {
                return (w, h);
            }
        }
        self.window
            .as_ref()
            .and_then(|w| w.current_monitor())
            .map(|m| (m.size().width as f64 * 0.95, m.size().height as f64 * 0.9))
            .unwrap_or((1920.0, 1080.0))
    }
}
