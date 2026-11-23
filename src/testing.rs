// Test script commands (LUMARIS_AUTOTEST=<file>, one command per line).
use crate::*;

impl App {
    pub(crate) fn test_command(&mut self, event_loop: &ActiveEventLoop, cmd: &str) {
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
                self.window_event(
                    event_loop,
                    self.window.as_ref().unwrap().id(),
                    WindowEvent::CursorMoved { device_id: winit::event::DeviceId::dummy(), position: PhysicalPosition::new(x, y) },
                );
                if a[0] != "move" {
                    let button = if a[0] == "rclick" { MouseButton::Right } else { MouseButton::Left };
                    for state in [ElementState::Pressed, ElementState::Released] {
                        self.window_event(
                            event_loop,
                            self.window.as_ref().unwrap().id(),
                            WindowEvent::MouseInput { device_id: winit::event::DeviceId::dummy(), state, button },
                        );
                    }
                }
            }
            ["settings"] => self.open_settings(),
            // input on popup surfaces, as the popup thread sends it (window coordinates)
            ["pmove", x, y] | ["ppress", x, y] => {
                let (Ok(x), Ok(y)) = (x.parse::<f64>(), y.parse::<f64>()) else { return };
                self.user_event(event_loop, UserEvent::Popup(popup::PopupInput::Motion(x, y)));
                if a[0] == "ppress" {
                    self.user_event(event_loop, UserEvent::Popup(popup::PopupInput::Press(true)));
                }
            }
            ["prelease"] => self.user_event(event_loop, UserEvent::Popup(popup::PopupInput::Release)),
            ["settingswheel", steps] => {
                if let (Some(p), Ok(n)) = (self.settings_panel.as_mut(), steps.parse::<f64>())
                    && p.wheel(n)
                {
                    self.sync_overlays();
                }
            }
            ["settingsinfo"] => {
                println!(
                    "SETTINGS {} popups={:?} window={:?} frame={:?} next={:?}",
                    self.settings_panel.as_ref().map(|p| p.describe()).unwrap_or_default(),
                    self.popups.as_ref().map(|p| p.count()),
                    self.window.as_ref().map(|w| w.inner_size()),
                    self.kwin.as_ref().and_then(|k| k.frame()),
                    keys::current().keys(keys::Command::Next)
                );
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
                println!(
                    "INFO orient={} files={} menu={} index={} tex={:?} anim={:?} slideshow={:?} zoom={:.4} window={}x{} image_rect={:.1},{:.1},{:.1},{:.1} frame={:?}",
                    self.orient,
                    self.files.files.len(),
                    self.menu.is_some(),
                    self.files.index,
                    self.renderer.as_ref().and_then(|r| r.texture_size()),
                    self.animation.as_ref().map(|a| a.frame),
                    self.slideshow,
                    self.view.zoom,
                    s.width,
                    s.height,
                    r[0],
                    r[1],
                    r[2],
                    r[3],
                    self.kwin.as_ref().and_then(|k| k.frame())
                );
            }
            ["quit"] => event_loop.exit(),
            _ => eprintln!("lumaris: unknown test command: {cmd}"),
        }
    }
}
