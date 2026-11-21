// Lumaris - lean and fast image viewer for Linux
// SPDX-License-Identifier: GPL-2.0-or-later
mod apps;
mod blur;
mod decode;
mod desktop;
mod files;
mod gpu;
mod kwin;
mod menu;
mod raw;
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

/// The window is fitted to the image within this part of the screen.
const MAX_WINDOW_PERCENT: f64 = 80.0;
/// Widest window aspect ratio, unless the image itself is wider.
const MAX_WINDOW_ASPECT: f64 = 16.0 / 9.0;
/// Zoom factor of one mouse wheel step.
const WHEEL_STEP: f64 = 1.2;
/// Smallest window (the image is centred in it).
const MIN_WINDOW: (u32, u32) = (200, 150);
/// Background behind and around the image, and how much it covers the (blurred) desktop:
/// 0 = see-through, 1 = solid.
const BACKGROUND_COLOR: [f64; 3] = [0.0, 0.0, 0.0];
const BACKGROUND_OPACITY: f64 = 0.0;
/// Decoded images kept around the current one (in each direction).
const CACHE_RADIUS: usize = 2;

pub enum UserEvent {
    Decoded(PathBuf, Result<Arc<Decoded>, String>),
    /// SIGINT / SIGTERM: quit normally (unloads the KWin script)
    Quit,
    /// Test script command (LUMARIS_AUTOTEST=<file>, one command per line)
    Test(String),
    /// A vector image re-rendered for the current zoom (path, request number)
    VectorRendered(PathBuf, u64, Result<Arc<Decoded>, String>),
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
}

impl App {
    fn window_size_for(&self, w: u32, h: u32, screen: PhysicalSize<u32>) -> PhysicalSize<u32> {
        let (iw, ih) = (w.max(1) as f64, h.max(1) as f64);
        let mut aw = screen.width as f64 * MAX_WINDOW_PERCENT / 100.0;
        let ah = screen.height as f64 * MAX_WINDOW_PERCENT / 100.0;
        aw = aw.min(ah * MAX_WINDOW_ASPECT.max(iw / ih));
        let z = (aw / iw).min(ah / ih).min(1.0);
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
        if self.windowed().is_some() {
            let (iw, ih) = self.view.image_size();
            let size = self.window_size_for(iw as u32, ih as u32, self.screen_size(event_loop));
            self.apply_window_size(size);
        }
        self.view.fit();
        self.followed_zoom = self.view.zoom;
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
        if self.windowed().is_none() {
            return;
        }
        let (iw, ih) = self.view.image_size();
        let (mut mw, mh) = self.max_client_size(event_loop);
        mw = mw.min(mh * MAX_WINDOW_ASPECT.max(iw / ih));
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
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        format!("{name}{suffix} - Lumaris")
    }

    fn show(&mut self, event_loop: &ActiveEventLoop, path: PathBuf, img: Arc<Decoded>) {
        let (Some(window), Some(renderer)) = (self.window.clone(), self.renderer.as_mut()) else { return };
        renderer.set_image(&img);
        window.set_title(&Self::title_for(&path, ""));
        self.view.set_image(img.width, img.height);
        self.shown = Some(path);
        self.fit(event_loop);
        self.next_slide = self.slideshow.map(|d| Instant::now() + d);
        self.animation = (img.frames.len() > 1).then(|| Animation { due: Instant::now() + img.frames[0].delay, image: img.clone(), frame: 0 });
        self.vector_request += 1;
        self.vector_scale = img.pixel_width as f64 / img.width as f64;
        self.current = Some(img);
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
        for k in [self.neighbour(1), self.neighbour(-1)].into_iter().flatten() {
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
                d.min(n - d) <= CACHE_RADIUS
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

    fn open_menu(&mut self, at: (f64, f64)) {
        use menu::{Action, Item};
        let Some(window) = self.window.clone() else { return };
        let have_image = self.current.is_some();
        let file = self.files.current().to_path_buf();
        self.menu_apps = if have_image { apps::for_file(&file) } else { Vec::new() };
        let open_with = Item::sub("Open image with", self.menu_apps.iter().enumerate().map(|(i, a)| Item::new(&a.name, "", Action::OpenWith(i))).collect());
        let current = self.slideshow.map(|d| d.as_millis() as u64).unwrap_or(0);
        let slideshow = [(0, "Off", "Esc"), (1000, "1 second", "1"), (2000, "2 seconds", "2"), (3000, "3 seconds", "3"), (5000, "5 seconds", "5"), (10000, "10 seconds", "")]
            .into_iter()
            .map(|(ms, label, key)| Item::check(label, key, Action::Slideshow(ms), current == ms))
            .collect();
        let zoom = vec![
            Item::new("Fit to window", "Enter", Action::Fit),
            Item::new("Original size (100%)", "", Action::ActualSize),
            Item::new("Zoom in", "+", Action::ZoomIn),
            Item::new("Zoom out", "-", Action::ZoomOut),
        ];
        let image_item = |i: Item| if have_image { i } else { i.disabled() };
        let items = vec![
            open_with,
            image_item(Item::new("Open containing folder", "", Action::OpenFolder)),
            Item::separator(),
            image_item(Item::new("Copy image", "Ctrl+C", Action::CopyImage)),
            Item::new("Copy file path", "Ctrl+Shift+C", Action::CopyPath),
            Item::separator(),
            Item::new("Next image", "Right", Action::Next),
            Item::new("Previous image", "Left", Action::Previous),
            Item::sub("Slideshow", slideshow),
            Item::check("Loop through folder", "", Action::ToggleLoop, self.loop_folder),
            Item::separator(),
            Item::sub("Zoom", zoom),
            Item::check("Full screen", "F11", Action::Fullscreen, window.fullscreen().is_some()),
            Item::separator(),
            Item::new("Exit", "Esc", Action::Quit),
        ];
        let s = window.inner_size();
        self.menu = Some(menu::Menu::open(items, at, (s.width, s.height), window.scale_factor()));
        self.sync_menu();
    }

    fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            self.sync_menu();
        }
    }

    /// Shows the current menu state (or no menu)
    fn sync_menu(&mut self) {
        if let Some(r) = self.renderer.as_mut() {
            match self.menu.as_mut() {
                Some(m) => r.set_overlays(&m.layers()),
                None => r.set_overlays(&[]),
            }
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
            Action::ToggleLoop => self.loop_folder = !self.loop_folder,
            Action::Fit => self.fit(event_loop),
            Action::ActualSize => {
                self.view.zoom_to(1.0);
                self.redraw();
            }
            Action::ZoomIn | Action::ZoomOut => {
                let c = self.window_center();
                self.view.zoom_at(if action == Action::ZoomIn { WHEEL_STEP } else { 1.0 / WHEEL_STEP }, c);
                self.redraw();
            }
            Action::Fullscreen => self.toggle_fullscreen(),
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
                _ if m.version != v => self.sync_menu(),
                _ => {}
            }
            return;
        }
        if let (Key::Character(c), true) = (key.as_ref(), self.mods.control_key()) {
            if c.eq_ignore_ascii_case("c") {
                let action = if self.mods.shift_key() { menu::Action::CopyPath } else { menu::Action::CopyImage };
                self.run(event_loop, action);
                return;
            }
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
        if digit > 0 && (!shift || ctrl) {
            let ms = digit * if ctrl && shift { 10 } else if ctrl { 100 } else { 1000 };
            self.start_slideshow(Duration::from_millis(ms));
            return;
        }
        match key.as_ref() {
            Key::Named(NamedKey::ArrowRight | NamedKey::PageDown) => self.step(event_loop, 1),
            Key::Named(NamedKey::ArrowLeft | NamedKey::PageUp | NamedKey::Backspace) => self.step(event_loop, -1),
            Key::Named(NamedKey::Home) => self.go_to(event_loop, 0),
            Key::Named(NamedKey::End) => self.go_to(event_loop, self.files.files.len() - 1),
            Key::Named(NamedKey::F11) => self.toggle_fullscreen(),
            Key::Named(NamedKey::Escape) => {
                if self.slideshow.is_some() {
                    self.stop_slideshow();
                } else if self.window.as_ref().is_some_and(|w| w.fullscreen().is_some()) {
                    self.toggle_fullscreen();
                } else {
                    event_loop.exit();
                }
            }
            Key::Character("+" | "=") => {
                let c = self.window_center();
                self.view.zoom_at(WHEEL_STEP, c);
                self.redraw();
            }
            Key::Character("-") => {
                let c = self.window_center();
                self.view.zoom_at(1.0 / WHEEL_STEP, c);
                self.redraw();
            }
            // Space: fit <-> 100%, Enter: fit
            Key::Named(NamedKey::Space) => {
                if self.view.is_fit() && (self.view.zoom - 1.0).abs() > 1e-9 {
                    self.view.zoom_to(1.0);
                    self.redraw();
                } else {
                    self.fit(event_loop);
                }
            }
            Key::Named(NamedKey::Enter) => self.fit(event_loop),
            _ => {}
        }
    }
}

impl App {
    /// wheel <steps> <x> <y> | key <name> | info | quit
    fn test_command(&mut self, event_loop: &ActiveEventLoop, cmd: &str) {
        let a: Vec<&str> = cmd.split_whitespace().collect();
        match a.as_slice() {
            ["wheel", steps, x, y] => {
                let (Ok(s), Ok(x), Ok(y)) = (steps.parse::<f64>(), x.parse(), y.parse()) else { return };
                self.cursor = (x, y);
                self.view.zoom_at(WHEEL_STEP.powf(s), (x, y));
                self.redraw();
            }
            ["key", name] => {
                let key = match *name {
                    "Right" => Key::Named(NamedKey::ArrowRight),
                    "Left" => Key::Named(NamedKey::ArrowLeft),
                    "F11" => Key::Named(NamedKey::F11),
                    "Escape" => Key::Named(NamedKey::Escape),
                    "Space" => Key::Named(NamedKey::Space),
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
            ["apps"] => {
                for a in apps::for_file(self.files.current()) {
                    println!("APP {} ({})", a.name, a.id);
                }
            }
            ["info"] => {
                let s = self.window.as_ref().map(|w| w.inner_size()).unwrap_or_default();
                let r = self.view.image_rect();
                println!("INFO menu={} index={} tex={:?} anim={:?} slideshow={:?} zoom={:.4} window={}x{} image_rect={:.1},{:.1},{:.1},{:.1} frame={:?}", self.menu.is_some(), self.files.index, self.renderer.as_ref().and_then(|r| r.texture_size()), self.animation.as_ref().map(|a| a.frame), self.slideshow, self.view.zoom, s.width, s.height, r[0], r[1], r[2], r[3], self.kwin.as_ref().and_then(|k| k.frame()));
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
                self.step(event_loop, 1);
                if self.files.index == before {
                    self.stop_slideshow(); // last image and no looping (or a single image)
                }
            }
        }
        self.next_animation_frame();
        let next = [self.next_slide, self.animation.as_ref().map(|a| a.due)].into_iter().flatten().min();
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
            .with_transparent(BACKGROUND_OPACITY < 1.0);
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
        let mut renderer = match gpu::Renderer::new(self.gpu.take().expect("gpu"), window.clone()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("lumaris: {e}");
                event_loop.exit();
                return;
            }
        };
        if renderer.transparent() {
            self.blur = blur::Blur::enable(&window);
        }
        renderer.set_background(BACKGROUND_COLOR, BACKGROUND_OPACITY);
        if self.timing {
            eprintln!("lumaris: blur {}, KWin placement {}", self.blur.is_some(), self.kwin.is_some());
            eprintln!("lumaris: window and surface ready after {:.1} ms", self.started.elapsed().as_secs_f64() * 1000.0);
        }
        let s = window.inner_size();
        self.view.resize(s.width, s.height);
        self.window = Some(window);
        self.renderer = Some(renderer);
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
                self.close_menu();
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(s.width, s.height);
                }
                self.view.resize(s.width, s.height);
                self.redraw();
            }
            WindowEvent::RedrawRequested => {
                let animating = self.view.step(Instant::now());
                self.follow_zoom(event_loop);
                if let Some(r) = self.renderer.as_mut() {
                    r.render(self.view.image_rect(), self.view.zoom);
                }
                if !self.first_frame_done {
                    self.first_frame_done = true;
                    // after the first frame: prepare the menu (applications, font) in the background
                    apps::warm_up();
                    menu::warm_up();
                    if self.timing {
                        eprintln!("lumaris: first frame after {:.1} ms", self.started.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                if animating {
                    self.redraw();
                } else {
                    self.update_vector();
                }
            }
            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } => {
                if let Some(m) = self.menu.as_mut() {
                    self.cursor = (x, y);
                    if m.hover((x, y)) {
                        self.sync_menu();
                    }
                    return;
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
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Right, ElementState::Pressed) => {
                    self.drag = None;
                    self.open_menu(self.cursor);
                }
                (MouseButton::Left, ElementState::Pressed) => self.drag = Some(self.cursor),
                (MouseButton::Left, ElementState::Released) => self.drag = None,
                (MouseButton::Back, ElementState::Pressed) => self.step(event_loop, -1),
                (MouseButton::Forward, ElementState::Pressed) => self.step(event_loop, 1),
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => {
                self.close_menu();
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 60.0,
                };
                if steps != 0.0 {
                    self.view.zoom_at(WHEEL_STEP.powf(steps), self.cursor);
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
            _ => {}
        }
    }
}

fn main() {
    let started = Instant::now();
    // the placement script must be loaded before the window exists (it centres new windows)
    let kwin = std::thread::spawn(kwin::KWin::start);
    let Some(arg) = std::env::args_os().nth(1) else {
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
    let first = std::thread::spawn(move || decode::decode(&first_path).map(Arc::new));
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
        loop_folder: true,
        animation: None,
        proxy: event_loop.create_proxy(),
        vector_request: 0,
        vector_scale: 1.0,
        current: None,
        menu: None,
        menu_apps: Vec::new(),
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("lumaris: {e}");
        std::process::exit(1);
    }
}
