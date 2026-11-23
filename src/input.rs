// Keyboard commands, menu actions and user commands.
use crate::*;

impl App {
    pub(crate) fn user_command_for(&self, key: &Key) -> Option<usize> {
        self.user_commands.iter().position(|c| !c.key.is_empty() && keys::KeyMap::matches(&c.key, key, self.mods))
    }

    pub(crate) fn run_user_command(&mut self, i: usize) {
        if self.current.is_some()
            && let Some(c) = self.user_commands.get(i)
        {
            c.run(self.files.current());
        }
    }

    pub(crate) fn pan_step(&mut self, dx: f64, dy: f64) {
        let c = self.window_center();
        self.view.pan(dx * c.0 * 0.3, dy * c.1 * 0.3);
        self.redraw();
    }

    pub(crate) fn command(&mut self, event_loop: &ActiveEventLoop, cmd: keys::Command) {
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
            Wallpaper => self.run(event_loop, menu::Action::Wallpaper),
            Close => {
                if self.info.is_some() {
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

    pub(crate) fn run(&mut self, event_loop: &ActiveEventLoop, action: menu::Action) {
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
            Action::Wallpaper => self.set_wallpaper(file),
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

    pub(crate) fn key(&mut self, event_loop: &ActiveEventLoop, key: Key, code: Option<KeyCode>) {
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
            if p.wants_keys() {
                // waiting for a new key on the Keys page, or a question about one
                let outcome = p.key(&key, self.mods);
                return self.settings_outcome(outcome);
            }
            if keys::current().lookup(&key, self.mods) == Some(keys::Command::Help) {
                return self.toggle_help();
            }
            if key == Key::Named(NamedKey::Escape) {
                if p.escape() {
                    self.close_settings();
                } else {
                    self.sync_overlays();
                }
            }
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
            let ms = digit
                * if ctrl && shift {
                    10
                } else if ctrl {
                    100
                } else {
                    1000
                };
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
}
