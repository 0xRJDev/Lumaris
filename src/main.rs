// Lumaris - lean and fast image viewer for Linux
// SPDX-License-Identifier: GPL-2.0-or-later
mod apps;
mod archive;
mod blur;
mod browse;
mod commands;
mod decode;
mod desktop;
mod dnd;
mod files;
mod gpu;
mod info;
mod input;
mod instance;
mod jpeg;
mod keys;
mod kwin;
mod menu;
mod panel;
mod panels;
mod popup;
mod portal;
mod raw;
mod settings;
mod settings_ui;
mod testing;
mod toast;
mod trash;
mod ui;
mod view;
mod watch;
mod window_size;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use decode::{Decoded, Loader};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{Fullscreen, Window, WindowId};

/// Smallest window (the image is centred in it).
const MIN_WINDOW: (u32, u32) = (200, 150);

fn wheel_step() -> f64 {
    1.0 + settings::current().number("ZoomStep") / 100.0
}

fn max_aspect() -> f64 {
    settings::aspect_ratio(settings::current().choice("MaxWindowAspect"))
}

pub enum UserEvent {
    /// a decoded image (path, loader generation, result)
    Decoded(PathBuf, u64, Result<Arc<Decoded>, String>),
    /// SIGINT / SIGTERM: quit normally (unloads the KWin script)
    Quit,
    /// Test script command (LUMARIS_AUTOTEST=<file>, one command per line)
    Test(String),
    /// A vector image re-rendered for the current zoom (path, request number)
    VectorRendered(PathBuf, u64, Result<Arc<Decoded>, String>),
    /// open this file (dialog, drag and drop, another `lumaris` in single instance mode)
    Open(PathBuf),
    /// mouse input on a menu shown outside the window
    Popup(popup::PopupInput),
    /// files were added to or removed from this folder
    FolderChanged(PathBuf),
    /// a short message to show in the window
    Message(String),
}

/// The animation being played: image, current frame, when the next frame is due
struct Animation {
    image: Arc<Decoded>,
    frame: usize,
    due: Instant,
}

struct App {
    files: files::FileList,
    loader: Loader,
    gpu: Option<gpu::Gpu>,
    first: Option<Result<Arc<Decoded>, String>>,
    cache: Vec<(PathBuf, Arc<Decoded>)>,
    kwin: Option<kwin::KWin>,
    blur: Option<blur::Blur>, // dropped before the window
    window: Option<Arc<Window>>,
    renderer: Option<gpu::Renderer>,
    view: view::View,
    shown: Option<PathBuf>,
    cursor: (f64, f64),
    drag: Option<(f64, f64)>,
    started: Instant,
    timing: bool,
    first_frame_done: bool,
    followed_zoom: f64,
    mods: ModifiersState,
    /// Slideshow interval, and when the next image is due
    slideshow: Option<Duration>,
    next_slide: Option<Instant>,
    /// Next after the last image goes to the first one (and back)
    loop_folder: bool,
    animation: Option<Animation>,
    proxy: winit::event_loop::EventLoopProxy<UserEvent>,
    /// Vector image re-rendering: request number and the scale requested last
    vector_request: u64,
    vector_scale: f64,
    current: Option<Arc<Decoded>>,
    menu: Option<menu::Menu>,
    menu_apps: Vec<apps::DesktopApp>,
    settings_panel: Option<settings_ui::SettingsPanel>,
    /// window size before the settings window enlarged it
    size_before_settings: Option<PhysicalSize<u32>>,
    /// window size before the menu enlarged it
    size_before_menu: Option<PhysicalSize<u32>>,
    /// zooming with Shift+wheel: the window does not follow the zoom
    zoom_in_place: bool,
    /// menus on popup surfaces, so they can extend beyond the window (Wayland)
    popups: Option<popup::Popups>,
    /// the zoom shown briefly while zooming
    toast: toast::Toast,
    toast_zoom: f64,
    /// window size the panels were last laid out for
    overlay_size: (u32, u32),
    info: Option<info::InfoBox>,
    info_zoom: f64,
    /// view orientation: bits 0-1 rotation (90° steps clockwise), bit 2 flipped horizontally
    orient: u32,
    last_click: Option<(Instant, (f64, f64))>,
    user_commands: Vec<commands::UserCommand>,
    navigation_panel: bool,
    nav: panel::Panel,
    map_drag: bool,
    /// slideshow fade to a new image: when it started
    fade_start: Option<Instant>,
    /// watches the folder being browsed for new and removed images
    watch: watch::Watch,
    _instance: Option<instance::Instance>,
}

impl App {
    fn title_for(path: &Path, suffix: &str) -> String {
        let mut name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some(a) = archive::archive_of(path) {
            // a page of a comic archive: "comic.cbz › page.jpg"
            name = format!("{} › {name}", a.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        }
        format!("{name}{suffix} - Lumaris")
    }

    fn show(&mut self, event_loop: &ActiveEventLoop, path: PathBuf, img: Arc<Decoded>) {
        let (Some(window), Some(renderer)) = (self.window.clone(), self.renderer.as_mut()) else { return };
        let fade = self.slideshow.is_some() && settings::current().choice("Transition") == 1 && self.current.is_some();
        if fade {
            renderer.set_image_fading(&img);
            self.fade_start = Some(Instant::now());
        } else {
            renderer.set_image(&img);
            renderer.end_fade();
            self.fade_start = None;
        }
        self.nav.clear_image();
        window.set_title(&Self::title_for(&path, ""));
        let keep = settings::current().bool("KeepZoom") && self.shown.is_some() && !self.view.is_fit();
        self.shown = Some(path);
        self.orient = 0;
        if keep {
            self.view.replace_image(img.width, img.height);
            self.redraw();
        } else {
            self.view.set_image(img.width, img.height);
            self.fit(event_loop);
        }
        self.next_slide = self.slideshow.map(|d| Instant::now() + d);
        let play = settings::current().bool("PlayAnimations");
        self.animation =
            (play && img.frames.len() > 1).then(|| Animation { due: Instant::now() + img.frames[0].delay, image: img.clone(), frame: 0 });
        self.vector_request += 1;
        self.vector_scale = img.pixel_width as f64 / img.width as f64;
        self.current = Some(img);
        self.refresh_info();
    }

    /// Rotates or flips the view of the current image. `op` maps a point of the new view to the
    /// point of the old view (unit square).
    fn orient(&mut self, event_loop: &ActiveEventLoop, op: fn((f32, f32)) -> (f32, f32)) {
        // texture position shown at display position p for an orientation (as in the shader)
        let uv = |o: u32, p: (f32, f32)| {
            let d = if o & 4 != 0 { (1.0 - p.0, p.1) } else { p };
            match o & 3 {
                1 => (d.1, 1.0 - d.0),
                2 => (1.0 - d.0, 1.0 - d.1),
                3 => (1.0 - d.1, d.0),
                _ => d,
            }
        };
        let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)];
        let old = self.orient;
        if let Some(new) = (0..8).find(|&o| corners.iter().all(|&p| uv(o, p) == uv(old, op(p)))) {
            self.orient = new;
        }
        let rot = self.orient & 3;
        self.nav.clear_image();
        if let Some(img) = self.current.clone() {
            let (w, h) = if rot % 2 == 1 { (img.height, img.width) } else { (img.width, img.height) };
            self.view.set_image(w, h);
            self.fit(event_loop);
        }
    }

    /// Sets the image as the wallpaper (in the background: the desktop may take a moment).
    fn set_wallpaper(&mut self, file: PathBuf) {
        if self.current.is_none() {
            return;
        }
        if archive::archive_of(&file).is_some() {
            return self.message("Pages of an archive cannot be the wallpaper".into());
        }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let text = match desktop::set_wallpaper(&file) {
                Ok(()) => "Wallpaper set".to_string(),
                Err(e) => {
                    eprintln!("lumaris: cannot set the wallpaper: {e}");
                    format!("Wallpaper not set: {e}")
                }
            };
            let _ = proxy.send_event(UserEvent::Message(text));
        });
    }

    /// Vector images: render again at the displayed size once the zoom has settled.
    fn update_vector(&mut self) {
        let Some(img) = self.current.clone() else { return };
        let Some(tree) = img.vector.clone() else { return };
        let scale = self.view.zoom;
        let limit = (decode::MAX_VECTOR_PIXELS / (img.width as f64 * img.height as f64)).sqrt();
        let wanted = scale.min(limit);
        if (wanted / self.vector_scale - 1.0).abs() < 0.05 {
            return;
        }
        self.vector_scale = wanted;
        self.vector_request += 1;
        let (id, proxy, path) = (self.vector_request, self.proxy.clone(), self.files.current().to_path_buf());
        std::thread::spawn(move || {
            let r = decode::render_svg(tree, img.width, img.height, wanted).map(Arc::new);
            let _ = proxy.send_event(UserEvent::VectorRendered(path, id, r));
        });
    }

    fn next_animation_frame(&mut self) {
        let Some(a) = self.animation.as_mut() else { return };
        let now = Instant::now();
        if now < a.due {
            return;
        }
        a.frame = (a.frame + 1) % a.image.frames.len();
        let f = &a.image.frames[a.frame];
        // keep the timing, unless far behind (e.g. after the window was hidden)
        a.due = if now - a.due > f.delay { now + f.delay } else { a.due + f.delay };
        if let Some(r) = self.renderer.as_mut() {
            r.update_pixels(&f.rgba);
        }
        self.redraw();
    }

    fn redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Applies the settings that work without a restart. `old`: the previous settings.
    fn apply_settings(&mut self, old: Option<&settings::Settings>) {
        let s = settings::current().clone();
        self.view.configure(s.choice("FitMode") == 1, s.bool("SmoothZoom"));
        self.loop_folder = s.bool("LoopFolder");
        self.watch_folder();
        self.navigation_panel = s.bool("NavigationPanel");
        if let (Some(r), Some(w)) = (self.renderer.as_mut(), self.window.clone()) {
            r.set_pixel_art(s.bool("PixelArt"), s.number("PixelArtMaxSize") as u32);
            let mode = s.choice("BackgroundMode");
            let colour = settings::colour(s.choice("BackgroundColour"));
            let opacity = if mode <= 1 { s.number("BackgroundOpacity") / 100.0 } else { 1.0 };
            r.set_background(colour, opacity);
            r.set_checkerboard(mode == 3);
            let blur = mode == 0 && r.transparent();
            if blur && self.blur.is_none() {
                self.blur = blur::Blur::enable(&w);
            } else if !blur {
                self.blur = None;
            }
        }
        let changed = |k: &str| {
            old.is_none_or(|o| {
                let i = settings::DEFS.iter().position(|d| d.key == k).unwrap();
                o.get(i) != s.get(i)
            })
        };
        if changed("KeepAbove")
            && (old.is_some() || s.bool("KeepAbove"))
            && let Some(k) = &self.kwin
        {
            k.set_keep_above(s.bool("KeepAbove"));
        }
        if old.is_some()
            && changed("IgnoreHidden")
            && let Some(files) = self.files.reload()
        {
            self.files = files;
            self.files.sort(s.choice("SortOrder"), s.bool("SortReverse"));
            self.prefetch();
        } else if old.is_some() && (changed("SortOrder") || changed("SortReverse")) {
            self.files.sort(s.choice("SortOrder"), s.bool("SortReverse"));
            self.prefetch();
        } else if old.is_none() && (s.choice("SortOrder") != 0 || s.bool("SortReverse")) {
            self.files.sort(s.choice("SortOrder"), s.bool("SortReverse"));
        }
        if old.is_some() && (changed("RawMode") || changed("ColourProfiles") || changed("PlayAnimations")) {
            // decode again with the new settings
            self.cache.clear();
            self.shown = None;
            self.loader.invalidate();
            self.prefetch();
        }
        if old.is_some() && (changed("MaxWindowSize") || changed("MaxWindowAspect") || changed("FitMode")) && self.view.is_fit() {
            self.followed_zoom = -1.0;
        }
        self.redraw();
    }

    /// New settings from the settings window or the menu: save, apply.
    fn change_settings(&mut self, new: settings::Settings) {
        let old = settings::current().clone();
        if let Err(e) = new.save() {
            eprintln!("lumaris: cannot save the settings: {e}");
        }
        settings::replace(new);
        self.apply_settings(Some(&old));
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(due) = self.next_slide
            && Instant::now() >= due
        {
            self.next_slide = None;
            let before = self.files.index;
            let at_end = before + 1 == self.files.files.len() && settings::current().bool("SlideshowStopAtEnd");
            if !at_end {
                self.step(event_loop, 1);
            }
            if self.files.index == before {
                self.stop_slideshow(); // last image and no looping (or a single image)
            }
        }
        self.next_animation_frame();
        if self.nav.expire() | self.toast.expire() {
            self.sync_overlays();
        }
        let next = [self.next_slide, self.animation.as_ref().map(|a| a.due), self.nav.deadline(), self.toast.deadline()]
            .into_iter()
            .flatten()
            .min();
        match next {
            Some(due) => event_loop.set_control_flow(ControlFlow::WaitUntil(due)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let first = self.first.take().unwrap_or_else(|| Err("not loaded".into()));
        let path = self.files.current().to_path_buf();
        let size = match &first {
            Ok(img) => self.window_size_for(img.width, img.height, self.screen_size(event_loop)),
            Err(_) => PhysicalSize::new(800, 600),
        };
        let suffix = if first.is_err() { " (cannot open)" } else { "" };
        // transparent: the window background can show the (blurred) desktop
        let mut attrs =
            Window::default_attributes().with_title(Self::title_for(&path, suffix)).with_inner_size(size).with_transparent(true);
        #[cfg(target_os = "linux")]
        {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attrs = attrs.with_name("lumaris", "lumaris");
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("lumaris: cannot create the window: {e}");
                event_loop.exit();
                return;
            }
        };
        let renderer = match gpu::Renderer::new(self.gpu.take().expect("gpu"), window.clone()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("lumaris: {e}");
                event_loop.exit();
                return;
            }
        };
        if settings::current().bool("StartFullScreen") {
            window.set_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        if self.timing {
            eprintln!("lumaris: blur {}, KWin placement {}", self.blur.is_some(), self.kwin.is_some());
            eprintln!("lumaris: window and surface ready after {:.1} ms", self.started.elapsed().as_secs_f64() * 1000.0);
        }
        let s = window.inner_size();
        self.view.resize(s.width, s.height);
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.apply_settings(None);
        if let Some(w) = self.window.clone() {
            dnd::start(&w, self.proxy.clone());
            self.popups = popup::Popups::new(&w, self.proxy.clone());
        }
        match first {
            Ok(img) => {
                self.cache.push((path.clone(), img.clone()));
                self.show(event_loop, path, img);
            }
            Err(e) => eprintln!("lumaris: {}: {e}", path.display()),
        }
        self.prefetch();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let (path, result) = match event {
            UserEvent::Decoded(path, generation, result) => {
                if generation != self.loader.generation() {
                    return; // decoded with settings that changed since
                }
                (path, result)
            }
            UserEvent::Quit => return event_loop.exit(),
            UserEvent::Test(cmd) => return self.test_command(event_loop, &cmd),
            UserEvent::Popup(input) => {
                match input {
                    popup::PopupInput::Motion(x, y) => {
                        self.cursor = (x, y);
                        let changed = match self.menu.as_mut() {
                            Some(m) => m.hover((x, y)),
                            None => self.settings_panel.as_mut().is_some_and(|p| p.drag_to((x, y)) || p.hover((x, y))),
                        };
                        if changed {
                            self.sync_overlays();
                        }
                    }
                    popup::PopupInput::Press(left) => {
                        let at = self.cursor;
                        if self.menu.is_some() {
                            self.menu_click(event_loop, at, !left);
                        } else if left && !self.settings_panel.as_mut().is_some_and(|p| p.start_drag(at)) {
                            self.settings_click(at);
                        }
                    }
                    popup::PopupInput::Release => {
                        if let Some(p) = self.settings_panel.as_mut() {
                            p.end_drag();
                        }
                    }
                    popup::PopupInput::Wheel(steps) => {
                        if self.settings_panel.as_mut().is_some_and(|p| p.wheel(steps)) {
                            self.sync_overlays();
                        }
                    }
                }
                return;
            }
            UserEvent::Message(text) => {
                self.message(text);
                return;
            }
            UserEvent::FolderChanged(dir) => {
                self.folder_changed(event_loop, &dir);
                return;
            }
            UserEvent::Open(path) => {
                self.open_path(event_loop, &path);
                if let Some(k) = &self.kwin {
                    k.activate();
                }
                return;
            }
            UserEvent::VectorRendered(path, id, result) => {
                if let (Ok(img), true) = (result, id == self.vector_request && self.files.current() == path) {
                    if let Some(r) = self.renderer.as_mut() {
                        r.replace_vector_pixels(&img);
                    }
                    self.redraw();
                }
                return;
            }
        };
        let current = self.files.current() == path;
        match result {
            Ok(img) => {
                if !self.cache.iter().any(|(p, _)| *p == path) {
                    self.cache.push((path.clone(), img.clone()));
                    self.trim_cache();
                }
                if current && self.shown.as_ref() != Some(&path) {
                    self.show(event_loop, path, img);
                }
            }
            Err(e) => {
                eprintln!("lumaris: {}: {e}", path.display());
                if current {
                    if let Some(r) = self.renderer.as_mut() {
                        r.clear_image();
                    }
                    self.animation = None;
                    self.current = None;
                    if let Some(w) = &self.window {
                        w.set_title(&Self::title_for(&path, " (cannot open)"));
                    }
                    self.shown = Some(path);
                    self.redraw();
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(s) => {
                // resized by the user or the compositor (not the menu's own enlargement): close the menu
                if self.menu.as_ref().is_some_and(|m| m.window_size() != (s.width, s.height)) {
                    self.size_before_menu = None;
                    self.close_menu();
                }
                let bounds = self.settings_bounds();
                if let Some(p) = self.settings_panel.as_mut() {
                    p.resize((s.width, s.height), bounds);
                    self.sync_overlays();
                }
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(s.width, s.height);
                }
                self.view.resize(s.width, s.height);
                // panels placed relative to the window size
                if self.toast.deadline().is_some() || self.navigation_panel {
                    self.sync_overlays();
                }
                self.redraw();
            }
            WindowEvent::RedrawRequested => {
                let animating = self.view.step(Instant::now());
                self.follow_zoom(event_loop);
                // the window resized itself (following the zoom): place the panels again
                let size = self.window.as_ref().map(|w| w.inner_size()).map(|s| (s.width, s.height)).unwrap_or((1, 1));
                if size != self.overlay_size && (self.toast.deadline().is_some() || self.navigation_panel) {
                    self.sync_overlays();
                }
                // the zoom while zooming (not for fitting a new image)
                let target = self.view.target_zoom();
                if (target - self.toast_zoom).abs() > 1e-9 {
                    let user = !self.view.is_fit() && self.toast_zoom > 0.0;
                    self.toast_zoom = target;
                    if user {
                        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
                        let pct = target * 100.0;
                        let text = if pct < 10.0 { format!("{pct:.1}%") } else { format!("{pct:.0}%") };
                        self.toast.show(text, scale);
                        self.sync_overlays();
                    }
                }
                // zoom map follows zoom and panning
                if self.navigation_panel {
                    let size = self.window.as_ref().map(|w| w.inner_size()).map(|s| (s.width, s.height)).unwrap_or((1, 1));
                    self.nav.configure(size, self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0));
                    if self.nav.set_view(self.view.image_rect()) {
                        self.sync_overlays();
                    }
                }
                const FADE: f32 = 0.4;
                let fade = self.fade_start.map(|t| (t.elapsed().as_secs_f32() / FADE).min(1.0));
                if let Some(r) = self.renderer.as_mut() {
                    r.render(self.view.image_rect(), self.view.zoom, self.orient, fade.filter(|f| *f < 1.0));
                    if fade.is_some_and(|f| f >= 1.0) {
                        r.end_fade();
                        self.fade_start = None;
                    }
                }
                if self.fade_start.is_some() {
                    self.redraw();
                }
                if !self.first_frame_done {
                    self.first_frame_done = true;
                    // after the first frame: prepare the menu (applications, font) in the background
                    apps::warm_up();
                    ui::warm_up();
                    if self.timing {
                        eprintln!("lumaris: first frame after {:.1} ms", self.started.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                if animating {
                    self.redraw();
                } else {
                    self.update_vector();
                    if self.info.is_some() && (self.view.zoom - self.info_zoom).abs() > 1e-6 {
                        self.refresh_info();
                    }
                }
            }
            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } => {
                if let Some(m) = self.menu.as_mut() {
                    self.cursor = (x, y);
                    if m.hover((x, y)) {
                        self.sync_overlays();
                    }
                    return;
                }
                if let Some(p) = self.settings_panel.as_mut() {
                    self.cursor = (x, y);
                    if p.drag_to((x, y)) || p.hover((x, y)) {
                        self.sync_overlays();
                    }
                    return;
                }
                if self.map_drag {
                    if let Some((fx, fy)) = self.nav.map_position((x, y)) {
                        self.view.center_on(fx, fy);
                        self.redraw();
                    }
                    self.cursor = (x, y);
                    return;
                }
                if self.navigation_panel && self.nav.poke((x, y)) {
                    self.sync_overlays();
                }
                if let Some((lx, ly)) = self.drag {
                    self.view.pan(x - lx, y - ly);
                    self.drag = Some((x, y));
                    self.redraw();
                }
                self.cursor = (x, y);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: b @ (MouseButton::Left | MouseButton::Right), .. }
                if self.menu.is_some() =>
            {
                self.menu_click(event_loop, self.cursor, b == MouseButton::Right);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } if self.settings_panel.is_some() => {
                let at = self.cursor;
                if !self.settings_panel.as_mut().is_some_and(|p| p.start_drag(at)) {
                    self.settings_click(at);
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. }
                if self.settings_panel.as_ref().is_some_and(|p| p.dragging()) =>
            {
                if let Some(p) = self.settings_panel.as_mut() {
                    p.end_drag();
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.settings_panel.is_some() => {
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 60.0,
                };
                if self.settings_panel.as_mut().is_some_and(|p| p.wheel(steps)) {
                    self.sync_overlays();
                }
            }
            WindowEvent::MouseInput { .. } if self.settings_panel.is_some() => {}
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Right, ElementState::Pressed) => {
                    self.drag = None;
                    self.open_menu(self.cursor);
                }
                (MouseButton::Left, ElementState::Pressed) if self.navigation_panel && self.nav.click(self.cursor).is_some() => {
                    match self.nav.click(self.cursor) {
                        Some(Ok(cmd)) => self.command(event_loop, cmd),
                        Some(Err((fx, fy))) => {
                            self.map_drag = true;
                            self.view.center_on(fx, fy);
                            self.redraw();
                        }
                        None => {}
                    }
                }
                (MouseButton::Left, ElementState::Pressed) => {
                    // double click: full screen
                    let now = Instant::now();
                    let double = self.last_click.is_some_and(|(t, p)| {
                        now - t < Duration::from_millis(400) && (p.0 - self.cursor.0).abs() < 6.0 && (p.1 - self.cursor.1).abs() < 6.0
                    });
                    if double {
                        self.last_click = None;
                        self.toggle_fullscreen();
                    } else {
                        self.last_click = Some((now, self.cursor));
                        self.drag = Some(self.cursor);
                    }
                }
                (MouseButton::Left, ElementState::Released) => {
                    self.drag = None;
                    self.map_drag = false;
                }
                (MouseButton::Back, ElementState::Pressed) => self.step(event_loop, -1),
                (MouseButton::Forward, ElementState::Pressed) => self.step(event_loop, 1),
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => {
                self.close_menu();
                let shift = self.mods.shift_key();
                let (x, y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x as f64, y as f64),
                    MouseScrollDelta::PixelDelta(p) => (p.x / 60.0, p.y / 60.0),
                };
                // with Shift, KDE may turn the wheel into sideways scrolling
                let steps = if y == 0.0 && shift { x } else { y };
                // the wheel zooms or navigates (setting); Ctrl+wheel does the other
                let navigate = (settings::current().choice("WheelAction") == 1) != self.mods.control_key();
                if steps != 0.0 && navigate && !shift {
                    self.step(event_loop, if steps < 0.0 { 1 } else { -1 });
                } else if steps != 0.0 {
                    // Shift+wheel: zoom only the image, the window keeps its size
                    self.zoom_in_place = shift;
                    self.view.zoom_at(wheel_step().powf(steps), self.cursor);
                    self.redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let code = match event.physical_key {
                    PhysicalKey::Code(c) => Some(c),
                    _ => None,
                };
                self.key(event_loop, event.logical_key, code);
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            // clicking elsewhere on the desktop closes the menu
            WindowEvent::Focused(false) => self.close_menu(),
            _ => {}
        }
    }
}

fn main() {
    let started = Instant::now();
    // the placement script must be loaded before the window exists (it centres new windows)
    let kwin = std::thread::spawn(kwin::KWin::start);
    let arg = std::env::args_os().nth(1).or_else(|| {
        // started without a file: ask for one
        portal::pick_image(None).map(|p| p.into_os_string())
    });
    let Some(arg) = arg else {
        if std::env::args_os().nth(1).is_none() {
            std::process::exit(0); // the open dialog was cancelled
        }
        eprintln!("usage: lumaris <image or folder>");
        std::process::exit(2);
    };
    if arg == "--version" {
        println!("Lumaris {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // an image file: start decoding it at once, while its folder is listed
    let early = std::fs::canonicalize(&arg).ok().filter(|p| p.is_file() && !archive::is_archive(p)).map(|p| {
        let path = p.clone();
        (p, std::thread::spawn(move || decode::decode_for_display(&path)))
    });
    let Some(files) = files::FileList::new(Path::new(&arg)) else {
        eprintln!("lumaris: no images found: {}", Path::new(&arg).display());
        std::process::exit(1);
    };
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("lumaris: no display: {e}");
            std::process::exit(1);
        }
    };
    // single instance: hand the file to a running Lumaris
    let single = if settings::current().bool("SingleInstance") {
        match instance::claim(files.current(), event_loop.create_proxy()) {
            Some(instance::Instance::Forwarded) => {
                drop(kwin.join()); // unloads the placement script again
                return;
            }
            other => other,
        }
    } else {
        None
    };
    let quit = event_loop.create_proxy();
    let _ = ctrlc::set_handler(move || {
        let _ = quit.send_event(UserEvent::Quit);
    });
    if let Some(script) = std::env::var_os("LUMARIS_AUTOTEST") {
        let proxy = event_loop.create_proxy();
        let text = std::fs::read_to_string(script).unwrap_or_default();
        std::thread::spawn(move || {
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                if let Some(ms) = line.strip_prefix("wait ") {
                    std::thread::sleep(std::time::Duration::from_millis(ms.trim().parse().unwrap_or(0)));
                } else if proxy.send_event(UserEvent::Test(line.to_string())).is_err() {
                    break;
                }
            }
        });
    }
    // decode the first image while the GPU starts up
    let first = match early {
        Some((path, decoding)) if path == files.current() => decoding,
        _ => {
            let path = files.current().to_path_buf();
            std::thread::spawn(move || decode::decode_for_display(&path))
        }
    };
    let gpu = match pollster::block_on(gpu::Gpu::new()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("lumaris: no usable GPU: {e}");
            std::process::exit(1);
        }
    };
    decode::set_max_texture(gpu.max_texture_side());
    let timing = std::env::var_os("LUMARIS_DEBUG_TIMING").is_some();
    if timing {
        eprintln!("lumaris: GPU ready after {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
    }
    let first = first.join().unwrap_or_else(|_| Err("decoder crashed".into()));
    if timing {
        eprintln!("lumaris: first image decoded after {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
    }
    let threads = std::thread::available_parallelism().map(|n| n.get().clamp(1, 2)).unwrap_or(1);
    let mut app = App {
        files,
        loader: Loader::new(threads, event_loop.create_proxy()),
        watch: watch::Watch::start(event_loop.create_proxy()),
        gpu: Some(gpu),
        first: Some(first),
        cache: Vec::new(),
        kwin: kwin.join().ok().flatten(),
        blur: None,
        window: None,
        renderer: None,
        view: view::View::new(),
        shown: None,
        cursor: (0.0, 0.0),
        drag: None,
        started,
        timing,
        first_frame_done: false,
        followed_zoom: 0.0,
        mods: ModifiersState::empty(),
        slideshow: None,
        next_slide: None,
        loop_folder: settings::current().bool("LoopFolder"),
        animation: None,
        proxy: event_loop.create_proxy(),
        vector_request: 0,
        vector_scale: 1.0,
        current: None,
        menu: None,
        menu_apps: Vec::new(),
        settings_panel: None,
        size_before_settings: None,
        size_before_menu: None,
        zoom_in_place: false,
        popups: None,
        toast: toast::Toast::new(),
        toast_zoom: 0.0,
        overlay_size: (0, 0),
        info: None,
        info_zoom: 0.0,
        orient: 0,
        last_click: None,
        user_commands: commands::load(),
        navigation_panel: settings::current().bool("NavigationPanel"),
        nav: panel::Panel::new(),
        map_drag: false,
        fade_start: None,
        _instance: single,
    };
    let result = event_loop.run_app(&mut app);
    // clean up what outlives Lumaris (the KWin script, the blur), then end the process at once:
    // decoder threads may still be running, and tearing down C libraries under them crashes
    app.blur = None;
    app.kwin = None;
    desktop::wait_for_copies();
    if let Err(e) = result {
        eprintln!("lumaris: {e}");
        quit_now(1);
    }
    quit_now(0);
}

/// Ends the process without running exit handlers (see the end of main)
fn quit_now(code: i32) -> ! {
    unsafe extern "C" {
        fn _exit(code: i32) -> !;
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    unsafe { _exit(code) }
}
