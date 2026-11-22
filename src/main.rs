// Lumaris - lean and fast image viewer for Linux
// SPDX-License-Identifier: GPL-2.0-or-later
mod apps;
mod archive;
mod blur;
mod commands;
mod decode;
mod desktop;
mod dnd;
mod files;
mod gpu;
mod info;
mod instance;
mod keys;
mod kwin;
mod menu;
mod panel;
mod popup;
mod portal;
mod raw;
mod settings;
mod settings_ui;
mod toast;
mod trash;
mod ui;
mod view;

use std::collections::HashSet;
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
    Decoded(PathBuf, Result<Arc<Decoded>, String>),
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
    in_flight: HashSet<PathBuf>,
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
    info: Option<info::InfoBox>,
    info_zoom: f64,
    help: Option<info::InfoBox>,
    /// view orientation: bits 0-1 rotation (90° steps clockwise), bit 2 flipped horizontally
    orient: u32,
    last_click: Option<(Instant, (f64, f64))>,
    user_commands: Vec<commands::UserCommand>,
    navigation_panel: bool,
    nav: panel::Panel,
    map_drag: bool,
    /// slideshow fade to a new image: when it started
    fade_start: Option<Instant>,
    _instance: Option<instance::Instance>,
}

impl App {
    /// Zoom an image opens with: fitted into the opening area, small images enlarged to the
    /// smallest opening size.
    fn open_zoom(&self, w: u32, h: u32, screen: PhysicalSize<u32>) -> f64 {
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

    fn window_size_for(&self, w: u32, h: u32, screen: PhysicalSize<u32>) -> PhysicalSize<u32> {
        let (iw, ih) = (w.max(1) as f64, h.max(1) as f64);
        let z = self.open_zoom(w, h, screen);
        PhysicalSize::new(((iw * z).round() as u32).max(MIN_WINDOW.0), ((ih * z).round() as u32).max(MIN_WINDOW.1))
    }

    /// Largest window content size that keeps the window (with its frame) on the screen.
    fn max_client_size(&self, event_loop: &ActiveEventLoop) -> (f64, f64) {
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

    fn windowed(&self) -> Option<Arc<Window>> {
        self.window.clone().filter(|w| w.fullscreen().is_none() && !w.is_maximized())
    }

    fn apply_window_size(&mut self, size: PhysicalSize<u32>) {
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
    fn fit(&mut self, event_loop: &ActiveEventLoop) {
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
    fn follow_zoom(&mut self, event_loop: &ActiveEventLoop) {
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

    fn screen_size(&self, event_loop: &ActiveEventLoop) -> PhysicalSize<u32> {
        self.window
            .as_ref()
            .and_then(|w| w.current_monitor())
            .or_else(|| event_loop.primary_monitor())
            .or_else(|| event_loop.available_monitors().next())
            .map(|m| m.size())
            .unwrap_or(PhysicalSize::new(1920, 1080))
    }

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
        self.animation = (play && img.frames.len() > 1).then(|| Animation { due: Instant::now() + img.frames[0].delay, image: img.clone(), frame: 0 });
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

    fn move_to_trash(&mut self, event_loop: &ActiveEventLoop) {
        if self.current.is_none() {
            return;
        }
        let path = self.files.current().to_path_buf();
        if archive::archive_of(&path).is_some() {
            if let Some(w) = &self.window {
                w.set_title(&Self::title_for(&path, " (pages of an archive cannot be moved to the trash)"));
            }
            return;
        }
        match trash::trash(&path) {
            Ok(()) => {
                self.cache.retain(|(p, _)| *p != path);
                let i = self.files.index;
                self.files.files.remove(i);
                if self.files.files.is_empty() {
                    event_loop.exit();
                    return;
                }
                self.files.index = i.min(self.files.files.len() - 1);
                self.shown = None;
                let k = self.files.index;
                self.files.index = usize::MAX; // force go_to to show it
                self.go_to_forced(event_loop, k);
            }
            Err(e) => {
                eprintln!("lumaris: cannot move {} to the trash: {e}", path.display());
                if let Some(w) = &self.window {
                    w.set_title(&Self::title_for(&path, &format!(" (not moved to the trash: {e})")));
                }
            }
        }
    }

    fn go_to_forced(&mut self, event_loop: &ActiveEventLoop, index: usize) {
        self.files.index = index;
        let path = self.files.current().to_path_buf();
        if let Some((_, img)) = self.cache.iter().find(|(p, _)| *p == path) {
            let img = img.clone();
            self.show(event_loop, path, img);
        } else {
            self.request(path);
        }
        self.prefetch();
    }

    /// Opens another image (and its folder)
    fn open_path(&mut self, event_loop: &ActiveEventLoop, path: &Path) {
        let Some(files) = files::FileList::new(path) else {
            eprintln!("lumaris: no images found: {}", path.display());
            return;
        };
        self.files = files;
        let s = settings::current().clone();
        if s.choice("SortOrder") != 0 || s.bool("SortReverse") {
            self.files.sort(s.choice("SortOrder"), s.bool("SortReverse"));
        }
        self.cache.clear();
        self.shown = None;
        self.stop_slideshow();
        let k = self.files.index;
        self.go_to_forced(event_loop, k);
    }

    /// The open dialog (in the background); the chosen file arrives as UserEvent::Open
    fn open_dialog(&mut self) {
        let folder = self.files.files.get(self.files.index).and_then(|p| p.parent()).map(Path::to_path_buf);
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            if let Some(p) = portal::pick_image(folder.as_deref()) {
                let _ = proxy.send_event(UserEvent::Open(p));
            }
        });
    }

    fn toggle_navigation_panel(&mut self) {
        self.navigation_panel = !self.navigation_panel;
        self.sync_overlays();
    }

    fn user_command_for(&self, key: &Key) -> Option<usize> {
        self.user_commands.iter().position(|c| !c.key.is_empty() && keys::KeyMap::matches(&c.key, key, self.mods))
    }

    fn run_user_command(&mut self, i: usize) {
        if self.current.is_some() {
            if let Some(c) = self.user_commands.get(i) {
                c.run(self.files.current());
            }
        }
    }

    fn toggle_help(&mut self) {
        if self.help.take().is_some() {
            self.sync_overlays();
            return;
        }
        let km = keys::current();
        let mut lines: Vec<(String, String)> = keys::COMMANDS
            .iter()
            .map(|(cmd, _, desc, _)| (km.shortcuts(*cmd), desc.to_string()))
            .filter(|(k, _)| !k.is_empty())
            .collect();
        for (k, d) in [
            ("1 … 9", "Slideshow, seconds per image (Ctrl: tenths)"),
            ("Mouse wheel", "Zoom (Ctrl: next / previous image)"),
            ("Drag", "Move the image"),
            ("Double click", "Full screen"),
            ("Right click", "Menu"),
            ("Mouse back / forward", "Previous / next image"),
        ] {
            lines.push((k.into(), d.into()));
        }
        drop(km);
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
        self.help = Some(info::InfoBox::with_lines(lines, scale));
        self.sync_overlays();
    }

    fn pan_step(&mut self, dx: f64, dy: f64) {
        let c = self.window_center();
        self.view.pan(dx * c.0 * 0.3, dy * c.1 * 0.3);
        self.redraw();
    }

    fn command(&mut self, event_loop: &ActiveEventLoop, cmd: keys::Command) {
        use keys::Command::*;
        match cmd {
            Next => self.step(event_loop, 1),
            Previous => self.step(event_loop, -1),
            First => self.go_to(event_loop, 0),
            Last => self.go_to(event_loop, self.files.files.len() - 1),
            ZoomIn | ZoomOut => {
                self.zoom_in_place = false;
                let c = self.window_center();
                self.view.zoom_at(if cmd == ZoomIn { wheel_step() } else { 1.0 / wheel_step() }, c);
                self.redraw();
            }
            ToggleFit => {
                if self.view.is_fit() && (self.view.zoom - 1.0).abs() > 1e-9 {
                    self.view.zoom_to(1.0);
                    self.redraw();
                } else {
                    self.fit(event_loop);
                }
            }
            Fit => self.fit(event_loop),
            ActualSize => {
                self.view.zoom_to(1.0);
                self.redraw();
            }
            PanLeft => self.pan_step(1.0, 0.0),
            PanRight => self.pan_step(-1.0, 0.0),
            PanUp => self.pan_step(0.0, 1.0),
            PanDown => self.pan_step(0.0, -1.0),
            RotateRight => self.orient(event_loop, |p| (p.1, 1.0 - p.0)),
            RotateLeft => self.orient(event_loop, |p| (1.0 - p.1, p.0)),
            FlipHorizontal => self.orient(event_loop, |p| (1.0 - p.0, p.1)),
            FlipVertical => self.orient(event_loop, |p| (p.0, 1.0 - p.1)),
            FullScreen => self.toggle_fullscreen(),
            CopyImage => self.run(event_loop, menu::Action::CopyImage),
            CopyPath => self.run(event_loop, menu::Action::CopyPath),
            ImageInfo => self.toggle_info(),
            NavigationPanel => self.toggle_navigation_panel(),
            Settings => self.open_settings(),
            Help => self.toggle_help(),
            Open => self.open_dialog(),
            OpenFolder => self.run(event_loop, menu::Action::OpenFolder),
            Trash => self.move_to_trash(event_loop),
            Close => {
                if self.help.is_some() {
                    self.toggle_help();
                } else if self.info.is_some() {
                    self.toggle_info();
                } else if self.slideshow.is_some() {
                    self.stop_slideshow();
                } else if self.window.as_ref().is_some_and(|w| w.fullscreen().is_some()) {
                    self.toggle_fullscreen();
                } else {
                    event_loop.exit();
                }
            }
            Quit => event_loop.exit(),
        }
    }

    fn toggle_info(&mut self) {
        if self.info.take().is_none() {
            self.info_zoom = -1.0;
            self.info = Some(info::InfoBox::new(Path::new(""), None, (0, 0), 1.0, 1.0)); // replaced right away
            self.refresh_info();
        } else {
            self.sync_overlays();
        }
    }

    /// Rebuilds the image info box (new image or zoom)
    fn refresh_info(&mut self) {
        if self.info.is_none() {
            return;
        }
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
        self.info_zoom = self.view.zoom;
        self.info = Some(info::InfoBox::new(
            self.files.current(),
            self.current.as_deref(),
            (self.files.index, self.files.files.len()),
            self.view.zoom,
            scale,
        ));
        self.sync_overlays();
    }

    fn open_settings(&mut self) {
        self.close_menu();
        let Some(window) = self.window.clone() else { return };
        let scale = window.scale_factor();
        let (ww, wh) = settings_ui::SettingsPanel::wanted_size(scale);
        let s = window.inner_size();
        if self.windowed().is_some() && (s.width < ww || s.height < wh) {
            self.size_before_settings = Some(s);
            self.apply_window_size(PhysicalSize::new(s.width.max(ww), s.height.max(wh)));
        }
        let s = window.inner_size();
        self.settings_panel = Some(settings_ui::SettingsPanel::new(settings::current().clone(), (s.width, s.height), scale));
        self.sync_overlays();
    }

    fn close_settings(&mut self) {
        if self.settings_panel.take().is_some() {
            if let Some(size) = self.size_before_settings.take() {
                if self.windowed().is_some() {
                    self.apply_window_size(size);
                }
            }
            self.sync_overlays();
        }
    }

    fn settings_click(&mut self, at: (f64, f64)) {
        let Some(p) = self.settings_panel.as_mut() else { return };
        match p.click(at) {
            settings_ui::Outcome::Nothing => {}
            settings_ui::Outcome::Redraw => self.sync_overlays(),
            settings_ui::Outcome::Changed(s) => {
                self.change_settings(s);
                self.sync_overlays();
            }
            settings_ui::Outcome::Close => self.close_settings(),
            settings_ui::Outcome::EditFile(which) => {
                let path = match which {
                    1 => keys::KeyMap::write_file_if_missing(),
                    2 => commands::write_file_if_missing(),
                    _ => {
                        let path = settings::Settings::path();
                        if !path.exists() {
                            let _ = settings::current().save();
                        }
                        path
                    }
                };
                apps::spawn_detached(std::process::Command::new("xdg-open").arg(path));
                self.close_settings();
            }
        }
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

    fn go_to(&mut self, event_loop: &ActiveEventLoop, index: usize) {
        if index >= self.files.files.len() || (index == self.files.index && self.shown.is_some()) {
            return;
        }
        self.files.index = index;
        let path = self.files.current().to_path_buf();
        if let Some((_, img)) = self.cache.iter().find(|(p, _)| *p == path) {
            let img = img.clone();
            self.show(event_loop, path, img);
        } else {
            self.request(path);
        }
        self.prefetch();
    }

    fn request(&mut self, path: PathBuf) {
        if self.in_flight.insert(path.clone()) {
            self.loader.request(path);
        }
    }

    fn prefetch(&mut self) {
        let (i, n) = (self.files.index, self.files.files.len());
        let ahead = settings::current().number("Prefetch") as isize;
        let wanted: Vec<usize> = (1..=ahead).flat_map(|d| [self.neighbour(d), self.neighbour(-d)]).flatten().collect();
        for k in wanted {
            let p = self.files.files[k].clone();
            if !self.cache.iter().any(|(c, _)| *c == p) {
                self.request(p);
            }
        }
        // drop images far from the current one (the folder may loop)
        let files = &self.files.files;
        self.cache.retain(|(p, _)| {
            files.iter().position(|f| f == p).is_some_and(|k| {
                let d = k.abs_diff(i);
                d.min(n - d) <= ahead as usize + 1
            })
        });
    }

    /// Index `delta` images away, looping through the folder if enabled
    fn neighbour(&self, delta: isize) -> Option<usize> {
        let n = self.files.files.len() as isize;
        let k = self.files.index as isize + delta;
        if self.loop_folder {
            Some(k.rem_euclid(n) as usize)
        } else {
            (0..n).contains(&k).then_some(k as usize)
        }
    }

    fn step(&mut self, event_loop: &ActiveEventLoop, delta: isize) {
        if let Some(k) = self.neighbour(delta) {
            self.go_to(event_loop, k);
        }
    }

    fn start_slideshow(&mut self, interval: Duration) {
        if settings::current().bool("SlideshowFullScreen") && self.window.as_ref().is_some_and(|w| w.fullscreen().is_none()) {
            self.toggle_fullscreen();
        }
        self.slideshow = Some(interval);
        self.next_slide = Some(Instant::now() + interval);
    }

    fn stop_slideshow(&mut self) {
        self.slideshow = None;
        self.next_slide = None;
    }

    fn toggle_fullscreen(&self) {
        if let Some(w) = &self.window {
            w.set_fullscreen(if w.fullscreen().is_some() { None } else { Some(Fullscreen::Borderless(None)) });
        }
    }

    fn redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn window_center(&self) -> (f64, f64) {
        self.window.as_ref().map(|w| w.inner_size()).map(|s| (s.width as f64 / 2.0, s.height as f64 / 2.0)).unwrap_or((0.0, 0.0))
    }

    /// Applies the settings that work without a restart. `old`: the previous settings.
    fn apply_settings(&mut self, old: Option<&settings::Settings>) {
        let s = settings::current().clone();
        self.view.configure(s.choice("FitMode") == 1, s.bool("SmoothZoom"));
        self.loop_folder = s.bool("LoopFolder");
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
        let changed = |k: &str| old.is_none_or(|o| {
            let i = settings::DEFS.iter().position(|d| d.key == k).unwrap();
            o.get(i) != s.get(i)
        });
        if changed("KeepAbove") && (old.is_some() || s.bool("KeepAbove")) {
            if let Some(k) = &self.kwin {
                k.set_keep_above(s.bool("KeepAbove"));
            }
        }
        if old.is_some() && (changed("SortOrder") || changed("SortReverse")) {
            self.files.sort(s.choice("SortOrder"), s.bool("SortReverse"));
            self.prefetch();
        } else if old.is_none() && (s.choice("SortOrder") != 0 || s.bool("SortReverse")) {
            self.files.sort(s.choice("SortOrder"), s.bool("SortReverse"));
        }
        if old.is_some() && (changed("RawMode") || changed("ColourProfiles") || changed("PlayAnimations")) {
            // decode again with the new settings
            self.cache.clear();
            self.shown = None;
            let path = self.files.current().to_path_buf();
            self.request(path);
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

    fn open_menu(&mut self, at: (f64, f64)) {
        use menu::{Action, Item};
        let Some(window) = self.window.clone() else { return };
        let have_image = self.current.is_some();
        let file = self.files.current().to_path_buf();
        self.menu_apps = if have_image { apps::for_file(&file) } else { Vec::new() };
        let open_with = Item::sub("Open image with", self.menu_apps.iter().enumerate().map(|(i, a)| Item::new(&a.name, "", Action::OpenWith(i))).collect());
        let current = self.slideshow.map(|d| d.as_millis() as u64).unwrap_or(0);
        let default_s = settings::current().number("SlideshowInterval");
        let start_label = format!("Start ({default_s} s per image)");
        let mut slideshow: Vec<Item> = vec![Item::new(&start_label, "", Action::SlideshowStart), Item::separator()];
        slideshow.extend([(0, "Off", "Esc"), (1000, "1 second", "1"), (2000, "2 seconds", "2"), (3000, "3 seconds", "3"), (5000, "5 seconds", "5"), (10000, "10 seconds", "")]
            .into_iter()
            .map(|(ms, label, key)| Item::check(label, key, Action::Slideshow(ms), current == ms)));
        let km = keys::current();
        let zoom = vec![
            Item::new("Fit to window", &km.shortcut(keys::Command::Fit), Action::Fit),
            Item::new("Original size (100%)", &km.shortcut(keys::Command::ActualSize), Action::ActualSize),
            Item::new("Zoom in", &km.shortcut(keys::Command::ZoomIn), Action::ZoomIn),
            Item::new("Zoom out", &km.shortcut(keys::Command::ZoomOut), Action::ZoomOut),
        ];
        let image_item = |i: Item| if have_image { i } else { i.disabled() };
        let rotate = vec![
            image_item(Item::new("Rotate right", &km.shortcut(keys::Command::RotateRight), Action::RotateRight)),
            image_item(Item::new("Rotate left", &km.shortcut(keys::Command::RotateLeft), Action::RotateLeft)),
            image_item(Item::new("Flip horizontally", &km.shortcut(keys::Command::FlipHorizontal), Action::FlipHorizontal)),
            image_item(Item::new("Flip vertically", &km.shortcut(keys::Command::FlipVertical), Action::FlipVertical)),
        ];
        let user: Vec<Item> = self
            .user_commands
            .iter()
            .enumerate()
            .map(|(i, c)| image_item(Item::new(&c.name, &keys::KeyMap::display(&c.key), Action::UserCommand(i))))
            .collect();
        let items = vec![
            open_with,
            image_item(Item::new("Open containing folder", &km.shortcut(keys::Command::OpenFolder), Action::OpenFolder)),
            Item::new("Open image…", &km.shortcut(keys::Command::Open), Action::Open),
            Item::separator(),
            image_item(Item::new("Copy image", &km.shortcut(keys::Command::CopyImage), Action::CopyImage)),
            Item::new("Copy file path", &km.shortcut(keys::Command::CopyPath), Action::CopyPath),
            image_item(Item::new("Move to trash", &km.shortcut(keys::Command::Trash), Action::Trash)),
            Item::separator(),
            Item::new("Next image", &km.shortcut(keys::Command::Next), Action::Next),
            Item::new("Previous image", &km.shortcut(keys::Command::Previous), Action::Previous),
            Item::sub("Slideshow", slideshow),
            Item::check("Loop through folder", "", Action::ToggleLoop, self.loop_folder),
            Item::separator(),
            Item::sub("Zoom", zoom),
            Item::sub("Rotate", rotate),
            Item::check("Full screen", &km.shortcut(keys::Command::FullScreen), Action::Fullscreen, window.fullscreen().is_some()),
            Item::separator(),
            Item::check("Image info", &km.shortcut(keys::Command::ImageInfo), Action::ImageInfo, self.info.is_some()),
            Item::check("Navigation panel", &km.shortcut(keys::Command::NavigationPanel), Action::NavigationPanel, self.navigation_panel),
            Item::new("Keys…", &km.shortcut(keys::Command::Help), Action::Help),
            Item::new("Settings…", &km.shortcut(keys::Command::Settings), Action::Settings),
            Item::separator(),
            Item::new("Exit", &km.shortcut(keys::Command::Quit), Action::Quit),
        ];
        let mut items = items;
        if !user.is_empty() {
            items.insert(3, Item::sub("Commands", user)); // after the open entries
        }
        drop(km);
        // menus on popup surfaces may use the whole screen area around the window
        let scale = window.scale_factor();
        let s = window.inner_size();
        if let (Some(_), Some(f)) = (self.popups.as_ref(), self.kwin.as_ref().and_then(|k| k.frame())) {
            if window.fullscreen().is_none() {
                // a few pixels inside the screen edges (KWin centres layer surfaces that touch them)
                let e = 4.0;
                let b = (
                    ((-f.client.0 as f64 + e) * scale) as f32,
                    ((-f.client.1 as f64 + e) * scale) as f32,
                    (((f.area.0 - f.client.0) as f64 - e) * scale) as f32,
                    (((f.area.1 - f.client.1) as f64 - e) * scale) as f32,
                );
                self.menu = Some(menu::Menu::open_in(items, at, (s.width, s.height), b, scale));
                self.sync_overlays();
                return;
            }
        }
        // otherwise a window too small for the menu grows while the menu is open
        let mut at = at;
        let (nw, nh) = menu::Menu::needed_size(&items, window.scale_factor());
        if self.windowed().is_some() && (s.width < nw || s.height < nh) {
            let (mw, mh) = self.max_client_size_now();
            let new = PhysicalSize::new(s.width.max(nw).min(mw as u32).max(s.width), s.height.max(nh).min(mh as u32).max(s.height));
            self.size_before_menu = Some(s);
            self.apply_window_size(new);
            // the window grows around its centre: the pointer moves by half the growth
            let n = window.inner_size();
            at = (at.0 + (n.width as f64 - s.width as f64) / 2.0, at.1 + (n.height as f64 - s.height as f64) / 2.0);
            self.cursor = at;
        }
        let s = window.inner_size();
        self.menu = Some(menu::Menu::open(items, at, (s.width, s.height), window.scale_factor()));
        self.sync_overlays();
    }

    fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            if let Some(size) = self.size_before_menu.take() {
                if self.windowed().is_some() {
                    self.apply_window_size(size);
                }
            }
            self.sync_overlays();
        }
    }

    /// Largest window content size (without an event loop at hand)
    fn max_client_size_now(&self) -> (f64, f64) {
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

    /// Shows the current panels: image info, settings window, menu (on top)
    fn sync_overlays(&mut self) {
        let size = self.window.as_ref().map(|w| w.inner_size()).map(|s| (s.width, s.height)).unwrap_or((1, 1));
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
        self.nav.configure(size, scale);
        self.nav.set_view(self.view.image_rect());
        if self.navigation_panel && self.nav.wants_image() {
            if let Some(img) = self.current.clone() {
                self.nav.set_image(&img.rgba, img.pixel_width, img.pixel_height, self.orient);
            }
        }
        let show_nav = self.navigation_panel && self.settings_panel.is_none();
        let App { renderer, menu, settings_panel, info, help, nav, toast, .. } = self;
        if let Some(r) = renderer.as_mut() {
            let mut layers = Vec::new();
            if let Some(i) = info.as_mut() {
                layers.extend(i.layer(size));
            }
            if show_nav {
                layers.extend(nav.layers());
            }
            if let Some(t) = toast.layer(size) {
                layers.push(t);
            }
            if let Some(h) = help.as_mut() {
                layers.extend(h.layer(size));
            }
            if let Some(p) = settings_panel.as_mut() {
                layers.extend(p.layers());
            }
            let on_popups = self.popups.is_some() && menu.as_ref().is_some_and(|m| m.on_popups());
            if let Some(m) = menu.as_mut() {
                if on_popups {
                    let client = self.kwin.as_ref().and_then(|k| k.frame()).map(|f| f.client).unwrap_or((0, 0));
                    if let Some(p) = self.popups.as_mut() {
                        p.show(&m.layers(), scale, client);
                    }
                } else {
                    layers.extend(m.layers());
                }
            } else if let Some(p) = self.popups.as_mut() {
                p.hide();
            }
            r.set_overlays(&layers);
        }
        self.redraw();
    }

    fn run(&mut self, event_loop: &ActiveEventLoop, action: menu::Action) {
        use menu::Action;
        self.close_menu();
        let file = self.files.current().to_path_buf();
        match action {
            Action::OpenWith(i) => {
                if let Some(app) = self.menu_apps.get(i) {
                    apps::launch(app, &file);
                }
            }
            Action::OpenFolder => desktop::show_in_folder(file),
            Action::CopyImage => {
                if let Some(img) = self.current.clone() {
                    desktop::copy_image(img);
                }
            }
            Action::CopyPath => desktop::copy_text(file.to_string_lossy().into_owned()),
            Action::Next => self.step(event_loop, 1),
            Action::Previous => self.step(event_loop, -1),
            Action::Slideshow(0) => self.stop_slideshow(),
            Action::Slideshow(ms) => self.start_slideshow(Duration::from_millis(ms)),
            Action::ToggleLoop => {
                let mut s = settings::current().clone();
                let i = settings::DEFS.iter().position(|d| d.key == "LoopFolder").unwrap();
                s.set(i, settings::Value::Bool(!self.loop_folder));
                self.change_settings(s);
            }
            Action::Fit => self.fit(event_loop),
            Action::ActualSize => {
                self.view.zoom_to(1.0);
                self.redraw();
            }
            Action::ZoomIn | Action::ZoomOut => {
                let c = self.window_center();
                self.view.zoom_at(if action == Action::ZoomIn { wheel_step() } else { 1.0 / wheel_step() }, c);
                self.redraw();
            }
            Action::Fullscreen => self.toggle_fullscreen(),
            Action::ImageInfo => self.toggle_info(),
            Action::Settings => self.open_settings(),
            Action::SlideshowStart => {
                let s = settings::current().number("SlideshowInterval");
                self.start_slideshow(Duration::from_secs_f64(s));
            }
            Action::Choice(_) => {}
            Action::Open => self.open_dialog(),
            Action::Trash => self.move_to_trash(event_loop),
            Action::RotateRight => self.command(event_loop, keys::Command::RotateRight),
            Action::RotateLeft => self.command(event_loop, keys::Command::RotateLeft),
            Action::FlipHorizontal => self.command(event_loop, keys::Command::FlipHorizontal),
            Action::FlipVertical => self.command(event_loop, keys::Command::FlipVertical),
            Action::NavigationPanel => self.toggle_navigation_panel(),
            Action::Help => self.toggle_help(),
            Action::UserCommand(i) => {
                if let Some(c) = self.user_commands.get(i) {
                    c.run(&file);
                }
            }
            Action::Quit => event_loop.exit(),
        }
    }

    fn menu_click(&mut self, event_loop: &ActiveEventLoop, at: (f64, f64), right: bool) {
        let Some(m) = self.menu.as_mut() else { return };
        match m.click(at) {
            menu::Click::Run(a) => self.run(event_loop, a),
            menu::Click::Close => {
                self.close_menu();
                if right {
                    self.open_menu(at);
                }
            }
            menu::Click::Nothing => {}
        }
    }

    fn key(&mut self, event_loop: &ActiveEventLoop, key: Key, code: Option<KeyCode>) {
        if let Some(m) = self.menu.as_mut() {
            let name = match key.as_ref() {
                Key::Named(NamedKey::ArrowUp) => "Up",
                Key::Named(NamedKey::ArrowDown) => "Down",
                Key::Named(NamedKey::ArrowLeft) => "Left",
                Key::Named(NamedKey::ArrowRight) => "Right",
                Key::Named(NamedKey::Enter | NamedKey::Space) => "Enter",
                _ => {
                    self.close_menu(); // Esc and any other key
                    return;
                }
            };
            let v = m.version;
            match m.key(name) {
                menu::Click::Run(a) => self.run(event_loop, a),
                _ if m.version != v => self.sync_overlays(),
                _ => {}
            }
            return;
        }
        if let Some(p) = self.settings_panel.as_mut() {
            if key == Key::Named(NamedKey::Escape) {
                if p.escape() {
                    self.close_settings();
                } else {
                    self.sync_overlays();
                }
            }
            return;
        }
        if self.help.is_some() && key != Key::Named(NamedKey::F1) {
            // any key closes the help
            self.toggle_help();
            return;
        }
        // 1..9: slideshow with 1..9 seconds per image (Ctrl: tenths, Ctrl+Shift: hundredths)
        let digit = match code {
            Some(KeyCode::Digit1 | KeyCode::Numpad1) => 1,
            Some(KeyCode::Digit2 | KeyCode::Numpad2) => 2,
            Some(KeyCode::Digit3 | KeyCode::Numpad3) => 3,
            Some(KeyCode::Digit4 | KeyCode::Numpad4) => 4,
            Some(KeyCode::Digit5 | KeyCode::Numpad5) => 5,
            Some(KeyCode::Digit6 | KeyCode::Numpad6) => 6,
            Some(KeyCode::Digit7 | KeyCode::Numpad7) => 7,
            Some(KeyCode::Digit8 | KeyCode::Numpad8) => 8,
            Some(KeyCode::Digit9 | KeyCode::Numpad9) => 9,
            _ => 0,
        };
        let (ctrl, shift) = (self.mods.control_key(), self.mods.shift_key());
        if digit > 0 && (!shift || ctrl) && !self.mods.alt_key() {
            let ms = digit * if ctrl && shift { 10 } else if ctrl { 100 } else { 1000 };
            self.start_slideshow(Duration::from_millis(ms));
            return;
        }
        let cmd = keys::current().lookup(&key, self.mods);
        if let Some(cmd) = cmd {
            self.command(event_loop, cmd);
        } else if let Some(c) = self.user_command_for(&key) {
            self.run_user_command(c);
        }
    }

    fn test_command(&mut self, event_loop: &ActiveEventLoop, cmd: &str) {
        let a: Vec<&str> = cmd.split_whitespace().collect();
        match a.as_slice() {
            ["wheel", steps, x, y] | ["shiftwheel", steps, x, y] => {
                let (Ok(s), Ok(x), Ok(y)) = (steps.parse::<f64>(), x.parse(), y.parse()) else { return };
                self.cursor = (x, y);
                self.zoom_in_place = a[0] == "shiftwheel";
                self.view.zoom_at(wheel_step().powf(s), (x, y));
                self.redraw();
            }
            ["key", name] => {
                let key = match *name {
                    "Right" => Key::Named(NamedKey::ArrowRight),
                    "Left" => Key::Named(NamedKey::ArrowLeft),
                    "F11" => Key::Named(NamedKey::F11),
                    "Escape" => Key::Named(NamedKey::Escape),
                    "Space" => Key::Named(NamedKey::Space),
                    "Delete" => Key::Named(NamedKey::Delete),
                    "F1" => Key::Named(NamedKey::F1),
                    c => Key::Character(c.into()),
                };
                let code = match *name {
                    "1" => Some(KeyCode::Digit1),
                    "2" => Some(KeyCode::Digit2),
                    _ => None,
                };
                self.key(event_loop, key, code);
            }
            ["rclick", x, y] | ["lclick", x, y] | ["move", x, y] => {
                let (Ok(x), Ok(y)) = (x.parse::<f64>(), y.parse::<f64>()) else { return };
                self.window_event(event_loop, self.window.as_ref().unwrap().id(), WindowEvent::CursorMoved {
                    device_id: winit::event::DeviceId::dummy(),
                    position: PhysicalPosition::new(x, y),
                });
                if a[0] != "move" {
                    let button = if a[0] == "rclick" { MouseButton::Right } else { MouseButton::Left };
                    for state in [ElementState::Pressed, ElementState::Released] {
                        self.window_event(event_loop, self.window.as_ref().unwrap().id(), WindowEvent::MouseInput {
                            device_id: winit::event::DeviceId::dummy(),
                            state,
                            button,
                        });
                    }
                }
            }
            ["settings"] => self.open_settings(),
            ["settingswheel", steps] => {
                if let (Some(p), Ok(n)) = (self.settings_panel.as_mut(), steps.parse::<f64>()) {
                    if p.wheel(n) {
                        self.sync_overlays();
                    }
                }
            }
            ["menuinfo"] => {
                println!(
                    "MENU levels={:?} popups={:?} window={:?} frame={:?}",
                    self.menu.as_ref().map(|m| m.rects()),
                    self.popups.as_ref().map(|p| p.count()),
                    self.window.as_ref().map(|w| w.inner_size()),
                    self.kwin.as_ref().and_then(|k| k.frame())
                );
            }
            ["apps"] => {
                for a in apps::for_file(self.files.current()) {
                    println!("APP {} ({})", a.name, a.id);
                }
            }
            ["info"] => {
                let s = self.window.as_ref().map(|w| w.inner_size()).unwrap_or_default();
                let r = self.view.image_rect();
                println!("INFO orient={} files={} menu={} index={} tex={:?} anim={:?} slideshow={:?} zoom={:.4} window={}x{} image_rect={:.1},{:.1},{:.1},{:.1} frame={:?}", self.orient, self.files.files.len(), self.menu.is_some(), self.files.index, self.renderer.as_ref().and_then(|r| r.texture_size()), self.animation.as_ref().map(|a| a.frame), self.slideshow, self.view.zoom, s.width, s.height, r[0], r[1], r[2], r[3], self.kwin.as_ref().and_then(|k| k.frame()));
            }
            ["quit"] => event_loop.exit(),
            _ => eprintln!("lumaris: unknown test command: {cmd}"),
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(due) = self.next_slide {
            if Instant::now() >= due {
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
        }
        self.next_animation_frame();
        if self.nav.expire() | self.toast.expire() {
            self.sync_overlays();
        }
        let next = [self.next_slide, self.animation.as_ref().map(|a| a.due), self.nav.deadline(), self.toast.deadline()].into_iter().flatten().min();
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
        let mut attrs = Window::default_attributes()
            .with_title(Self::title_for(&path, suffix))
            .with_inner_size(size)
            .with_transparent(true);
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
            UserEvent::Decoded(path, result) => (path, result),
            UserEvent::Quit => return event_loop.exit(),
            UserEvent::Test(cmd) => return self.test_command(event_loop, &cmd),
            UserEvent::Popup(input) => {
                match input {
                    popup::PopupInput::Motion(x, y) => {
                        self.cursor = (x, y);
                        if self.menu.as_mut().is_some_and(|m| m.hover((x, y))) {
                            self.sync_overlays();
                        }
                    }
                    popup::PopupInput::Press(left) => {
                        let at = self.cursor;
                        self.menu_click(event_loop, at, !left);
                    }
                }
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
        self.in_flight.remove(&path);
        let current = self.files.current() == path;
        match result {
            Ok(img) => {
                if !self.cache.iter().any(|(p, _)| *p == path) {
                    self.cache.push((path.clone(), img.clone()));
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
                if let Some(p) = self.settings_panel.as_mut() {
                    p.resize((s.width, s.height));
                    self.sync_overlays();
                }
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(s.width, s.height);
                }
                self.view.resize(s.width, s.height);
                // panels placed relative to the window size
                if self.help.is_some() || self.toast.deadline().is_some() || self.navigation_panel {
                    self.sync_overlays();
                }
                self.redraw();
            }
            WindowEvent::RedrawRequested => {
                let animating = self.view.step(Instant::now());
                self.follow_zoom(event_loop);
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
                    if p.hover((x, y)) {
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
                self.settings_click(self.cursor);
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
    let first_path = files.current().to_path_buf();
    let first = std::thread::spawn(move || {
        std::panic::catch_unwind(|| decode::decode(&first_path)).unwrap_or_else(|_| Err("the file could not be decoded".into())).map(Arc::new)
    });
    let gpu = match pollster::block_on(gpu::Gpu::new()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("lumaris: no usable GPU: {e}");
            std::process::exit(1);
        }
    };
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
        gpu: Some(gpu),
        first: Some(first),
        cache: Vec::new(),
        in_flight: HashSet::new(),
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
        info: None,
        info_zoom: 0.0,
        help: None,
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
