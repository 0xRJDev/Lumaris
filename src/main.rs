// Lumae - lean and fast image viewer for Linux
// SPDX-License-Identifier: GPL-2.0-or-later
mod blur;
mod decode;
mod files;
mod gpu;
mod kwin;
mod view;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use decode::{Decoded, Loader};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
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
    /// Test script command (LUMAE_AUTOTEST=<file>, one command per line)
    Test(String),
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
        format!("{name}{suffix} - Lumae")
    }

    fn show(&mut self, event_loop: &ActiveEventLoop, path: PathBuf, img: Arc<Decoded>) {
        let (Some(window), Some(renderer)) = (self.window.clone(), self.renderer.as_mut()) else { return };
        renderer.set_image(&img);
        window.set_title(&Self::title_for(&path, ""));
        self.view.set_image(img.width, img.height);
        self.shown = Some(path);
        self.fit(event_loop);
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
        let i = self.files.index;
        let neighbours = [i + 1, i.wrapping_sub(1)];
        for n in neighbours {
            if let Some(p) = self.files.files.get(n).cloned() {
                if !self.cache.iter().any(|(c, _)| *c == p) {
                    self.request(p);
                }
            }
        }
        // drop images far from the current one
        let files = &self.files.files;
        self.cache.retain(|(p, _)| files.iter().position(|f| f == p).is_some_and(|k| k.abs_diff(i) <= CACHE_RADIUS));
    }

    fn step(&mut self, event_loop: &ActiveEventLoop, delta: isize) {
        let n = self.files.files.len() as isize;
        let target = (self.files.index as isize + delta).clamp(0, n - 1) as usize;
        self.go_to(event_loop, target);
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

    fn key(&mut self, event_loop: &ActiveEventLoop, key: Key) {
        match key.as_ref() {
            Key::Named(NamedKey::ArrowRight | NamedKey::PageDown | NamedKey::Space) => self.step(event_loop, 1),
            Key::Named(NamedKey::ArrowLeft | NamedKey::PageUp | NamedKey::Backspace) => self.step(event_loop, -1),
            Key::Named(NamedKey::Home) => self.go_to(event_loop, 0),
            Key::Named(NamedKey::End) => self.go_to(event_loop, self.files.files.len() - 1),
            Key::Named(NamedKey::F11) => self.toggle_fullscreen(),
            Key::Named(NamedKey::Escape) => {
                if self.window.as_ref().is_some_and(|w| w.fullscreen().is_some()) {
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
            Key::Character("0" | "f" | "F") => self.fit(event_loop),
            Key::Character("1") => {
                self.view.zoom_to(1.0);
                self.redraw();
            }
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
                    c => Key::Character(c.into()),
                };
                self.key(event_loop, key);
            }
            ["info"] => {
                let s = self.window.as_ref().map(|w| w.inner_size()).unwrap_or_default();
                let r = self.view.image_rect();
                println!("INFO zoom={:.4} window={}x{} image_rect={:.1},{:.1},{:.1},{:.1} frame={:?}", self.view.zoom, s.width, s.height, r[0], r[1], r[2], r[3], self.kwin.as_ref().and_then(|k| k.frame()));
            }
            ["quit"] => event_loop.exit(),
            _ => eprintln!("lumae: unknown test command: {cmd}"),
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
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
            attrs = attrs.with_name("lumae", "lumae");
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("lumae: cannot create the window: {e}");
                event_loop.exit();
                return;
            }
        };
        let mut renderer = match gpu::Renderer::new(self.gpu.take().expect("gpu"), window.clone()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("lumae: {e}");
                event_loop.exit();
                return;
            }
        };
        if renderer.transparent() {
            self.blur = blur::Blur::enable(&window);
        }
        renderer.set_background(BACKGROUND_COLOR, BACKGROUND_OPACITY);
        if self.timing {
            eprintln!("lumae: blur {}, KWin placement {}", self.blur.is_some(), self.kwin.is_some());
            eprintln!("lumae: window and surface ready after {:.1} ms", self.started.elapsed().as_secs_f64() * 1000.0);
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
            Err(e) => eprintln!("lumae: {}: {e}", path.display()),
        }
        self.prefetch();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let (path, result) = match event {
            UserEvent::Decoded(path, result) => (path, result),
            UserEvent::Quit => return event_loop.exit(),
            UserEvent::Test(cmd) => return self.test_command(event_loop, &cmd),
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
                eprintln!("lumae: {}: {e}", path.display());
                if current {
                    if let Some(r) = self.renderer.as_mut() {
                        r.clear_image();
                    }
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
                    if self.timing {
                        eprintln!("lumae: first frame after {:.1} ms", self.started.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                if animating {
                    self.redraw();
                }
            }
            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } => {
                if let Some((lx, ly)) = self.drag {
                    self.view.pan(x - lx, y - ly);
                    self.drag = Some((x, y));
                    self.redraw();
                }
                self.cursor = (x, y);
            }
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => self.drag = Some(self.cursor),
                (MouseButton::Left, ElementState::Released) => self.drag = None,
                (MouseButton::Back, ElementState::Pressed) => self.step(event_loop, -1),
                (MouseButton::Forward, ElementState::Pressed) => self.step(event_loop, 1),
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => {
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
                self.key(event_loop, event.logical_key);
            }
            _ => {}
        }
    }
}

fn main() {
    let started = Instant::now();
    // the placement script must be loaded before the window exists (it centres new windows)
    let kwin = std::thread::spawn(kwin::KWin::start);
    let Some(arg) = std::env::args_os().nth(1) else {
        eprintln!("usage: lumae <image or folder>");
        std::process::exit(2);
    };
    if arg == "--version" {
        println!("Lumae {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let Some(files) = files::FileList::new(Path::new(&arg)) else {
        eprintln!("lumae: no images found: {}", Path::new(&arg).display());
        std::process::exit(1);
    };
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("lumae: no display: {e}");
            std::process::exit(1);
        }
    };
    let quit = event_loop.create_proxy();
    let _ = ctrlc::set_handler(move || {
        let _ = quit.send_event(UserEvent::Quit);
    });
    if let Some(script) = std::env::var_os("LUMAE_AUTOTEST") {
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
            eprintln!("lumae: no usable GPU: {e}");
            std::process::exit(1);
        }
    };
    let timing = std::env::var_os("LUMAE_DEBUG_TIMING").is_some();
    if timing {
        eprintln!("lumae: GPU ready after {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
    }
    let first = first.join().unwrap_or_else(|_| Err("decoder crashed".into()));
    if timing {
        eprintln!("lumae: first image decoded after {:.1} ms", started.elapsed().as_secs_f64() * 1000.0);
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
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("lumae: {e}");
        std::process::exit(1);
    }
}
