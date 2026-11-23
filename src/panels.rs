// Panels and menus: image info, navigation panel, settings window (Keys page), right-click
// menu, messages; drawn in the window or on popup surfaces.
use crate::*;

impl App {
    /// A short message at the bottom of the window
    pub(crate) fn message(&mut self, text: String) {
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
        self.toast.message(text, scale);
        self.sync_overlays();
    }

    pub(crate) fn toggle_navigation_panel(&mut self) {
        self.navigation_panel = !self.navigation_panel;
        self.sync_overlays();
    }

    /// F1: the Keys page of the settings (closes it when it is shown)
    pub(crate) fn toggle_help(&mut self) {
        if self.settings_panel.as_ref().is_some_and(|p| p.on_keys()) {
            return self.close_settings();
        }
        if self.settings_panel.is_none() {
            self.open_settings();
        }
        if let Some(p) = self.settings_panel.as_mut() {
            p.open_keys();
        }
        self.sync_overlays();
    }

    pub(crate) fn toggle_info(&mut self) {
        if self.info.take().is_none() {
            self.info_zoom = -1.0;
            self.info = Some(info::InfoBox::new(Path::new(""), None, (0, 0), 1.0, 1.0)); // replaced right away
            self.refresh_info();
        } else {
            self.sync_overlays();
        }
    }

    /// Rebuilds the image info box (new image or zoom)
    pub(crate) fn refresh_info(&mut self) {
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

    /// Area of the settings window: the screen around the window, or the window itself
    pub(crate) fn settings_bounds(&self) -> (f32, f32, f32, f32) {
        let s = self.window.as_ref().map(|w| w.inner_size()).unwrap_or_default();
        self.screen_bounds().unwrap_or((0.0, 0.0, s.width as f32, s.height as f32))
    }

    pub(crate) fn open_settings(&mut self) {
        self.close_menu();
        let Some(window) = self.window.clone() else { return };
        let scale = window.scale_factor();
        let (ww, wh) = settings_ui::SettingsPanel::wanted_size(scale);
        let s = window.inner_size();
        // shown beyond the window when possible; otherwise a small window grows while it is open
        if self.screen_bounds().is_none() && self.windowed().is_some() && (s.width < ww || s.height < wh) {
            self.size_before_settings = Some(s);
            self.apply_window_size(PhysicalSize::new(s.width.max(ww), s.height.max(wh)));
        }
        let s = window.inner_size();
        let user_keys = self.user_commands.iter().map(|c| (c.name.clone(), c.key.clone())).collect();
        let bounds = self.settings_bounds();
        self.settings_panel =
            Some(settings_ui::SettingsPanel::new(settings::current().clone(), user_keys, (s.width, s.height), bounds, scale));
        self.sync_overlays();
    }

    pub(crate) fn close_settings(&mut self) {
        if self.settings_panel.take().is_some() {
            if let Some(size) = self.size_before_settings.take()
                && self.windowed().is_some()
            {
                self.apply_window_size(size);
            }
            self.sync_overlays();
        }
    }

    pub(crate) fn settings_click(&mut self, at: (f64, f64)) {
        let Some(p) = self.settings_panel.as_mut() else { return };
        let outcome = p.click(at);
        self.settings_outcome(outcome);
    }

    pub(crate) fn settings_outcome(&mut self, outcome: settings_ui::Outcome) {
        match outcome {
            settings_ui::Outcome::Nothing => {}
            settings_ui::Outcome::Redraw => self.sync_overlays(),
            settings_ui::Outcome::Changed(s) => {
                self.change_settings(s);
                self.sync_overlays();
            }
            settings_ui::Outcome::KeysChanged(map) => {
                keys::replace(map);
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

    pub(crate) fn open_menu(&mut self, at: (f64, f64)) {
        use menu::{Action, Item};
        let Some(window) = self.window.clone() else { return };
        let have_image = self.current.is_some();
        let file = self.files.current().to_path_buf();
        self.menu_apps = if have_image { apps::for_file(&file) } else { Vec::new() };
        let open_with = Item::sub(
            "Open image with",
            self.menu_apps.iter().enumerate().map(|(i, a)| Item::new(&a.name, "", Action::OpenWith(i))).collect(),
        );
        let current = self.slideshow.map(|d| d.as_millis() as u64).unwrap_or(0);
        let default_s = settings::current().number("SlideshowInterval");
        let start_label = format!("Start ({default_s} s per image)");
        let mut slideshow: Vec<Item> = vec![Item::new(&start_label, "", Action::SlideshowStart), Item::separator()];
        slideshow.extend(
            [
                (0, "Off", "Esc"),
                (1000, "1 second", "1"),
                (2000, "2 seconds", "2"),
                (3000, "3 seconds", "3"),
                (5000, "5 seconds", "5"),
                (10000, "10 seconds", ""),
            ]
            .into_iter()
            .map(|(ms, label, key)| Item::check(label, key, Action::Slideshow(ms), current == ms)),
        );
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
            image_item(Item::new("Set as wallpaper", &km.shortcut(keys::Command::Wallpaper), Action::Wallpaper)),
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
        if let Some(b) = self.screen_bounds() {
            self.menu = Some(menu::Menu::open_in(items, at, (s.width, s.height), b, scale));
            self.sync_overlays();
            return;
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

    pub(crate) fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            if let Some(size) = self.size_before_menu.take()
                && self.windowed().is_some()
            {
                self.apply_window_size(size);
            }
            self.sync_overlays();
        }
    }

    /// Shows the current panels: image info, settings window, menu (on top)
    pub(crate) fn sync_overlays(&mut self) {
        let size = self.window.as_ref().map(|w| w.inner_size()).map(|s| (s.width, s.height)).unwrap_or((1, 1));
        self.overlay_size = size;
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
        self.nav.configure(size, scale);
        self.nav.set_view(self.view.image_rect());
        if self.navigation_panel
            && self.nav.wants_image()
            && let Some(img) = self.current.clone()
        {
            self.nav.set_image(&img.rgba, img.pixel_width, img.pixel_height, self.orient);
        }
        let show_nav = self.navigation_panel && self.settings_panel.is_none();
        let App { renderer, menu, settings_panel, info, nav, toast, .. } = self;
        if let Some(r) = renderer.as_mut() {
            let mut layers = Vec::new();
            if let Some(i) = info.as_mut() {
                layers.extend(i.layer());
            }
            if show_nav {
                layers.extend(nav.layers());
            }
            if let Some(t) = toast.layer(size) {
                layers.push(t);
            }
            // the settings window and the menu are drawn on popup surfaces when they extend
            // beyond the window, otherwise in it
            let mut popup_layers = Vec::new();
            if let Some(p) = settings_panel.as_mut() {
                if self.popups.is_some() && p.on_popups() {
                    popup_layers.extend(p.layers());
                } else {
                    layers.extend(p.layers());
                }
            }
            if let Some(m) = menu.as_mut() {
                if self.popups.is_some() && m.on_popups() {
                    popup_layers.extend(m.layers());
                } else {
                    layers.extend(m.layers());
                }
            }
            if let Some(p) = self.popups.as_mut() {
                if popup_layers.is_empty() {
                    p.hide();
                } else {
                    let client = self.kwin.as_ref().and_then(|k| k.frame()).map(|f| f.client).unwrap_or((0, 0));
                    p.show(&popup_layers, scale, client);
                }
            }
            r.set_overlays(&layers);
        }
        self.redraw();
    }

    pub(crate) fn menu_click(&mut self, event_loop: &ActiveEventLoop, at: (f64, f64), right: bool) {
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
}
