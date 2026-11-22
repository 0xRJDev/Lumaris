// Key bindings: commands with default keys, changeable in ~/.config/lumaris/keys.conf.
// The digits 1..9 always start a slideshow (Ctrl: tenths of a second).
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Next,
    Previous,
    First,
    Last,
    ZoomIn,
    ZoomOut,
    ToggleFit,
    Fit,
    ActualSize,
    PanLeft,
    PanRight,
    PanUp,
    PanDown,
    RotateRight,
    RotateLeft,
    FlipHorizontal,
    FlipVertical,
    FullScreen,
    CopyImage,
    CopyPath,
    ImageInfo,
    NavigationPanel,
    Settings,
    Help,
    Open,
    OpenFolder,
    Trash,
    Wallpaper,
    Close,
    Quit,
}

use Command::*;

/// command, name in keys.conf, description (help), default keys
pub const COMMANDS: &[(Command, &str, &str, &[&str])] = &[
    (Next, "Next", "Next image", &["Right", "PageDown"]),
    (Previous, "Previous", "Previous image", &["Left", "PageUp", "Backspace"]),
    (First, "First", "First image", &["Home"]),
    (Last, "Last", "Last image", &["End"]),
    (ZoomIn, "ZoomIn", "Zoom in", &["Plus", "Equal"]),
    (ZoomOut, "ZoomOut", "Zoom out", &["Minus"]),
    (ToggleFit, "ToggleFit", "Fit to window / 100%", &["Space"]),
    (Fit, "Fit", "Fit to window", &["Enter"]),
    (ActualSize, "ActualSize", "Original size (100%)", &["0"]),
    (PanLeft, "PanLeft", "Move the image left (zoomed in)", &["Ctrl+Left"]),
    (PanRight, "PanRight", "Move the image right (zoomed in)", &["Ctrl+Right"]),
    (PanUp, "PanUp", "Move the image up (zoomed in)", &["Up"]),
    (PanDown, "PanDown", "Move the image down (zoomed in)", &["Down"]),
    (RotateRight, "RotateRight", "Rotate right", &["R"]),
    (RotateLeft, "RotateLeft", "Rotate left", &["Shift+R"]),
    (FlipHorizontal, "FlipHorizontal", "Flip horizontally", &["H"]),
    (FlipVertical, "FlipVertical", "Flip vertically", &["V"]),
    (FullScreen, "FullScreen", "Full screen", &["F11", "F"]),
    (CopyImage, "CopyImage", "Copy image", &["Ctrl+C"]),
    (CopyPath, "CopyPath", "Copy file path", &["Ctrl+Shift+C"]),
    (ImageInfo, "ImageInfo", "Image info", &["I"]),
    (NavigationPanel, "NavigationPanel", "Navigation panel on / off", &["N"]),
    (Settings, "Settings", "Settings", &["Ctrl+Comma"]),
    (Help, "Help", "Keys (this help)", &["F1"]),
    (Open, "Open", "Open image", &["Ctrl+O"]),
    (OpenFolder, "OpenFolder", "Open containing folder", &["Ctrl+E"]),
    (Trash, "Trash", "Move to trash", &["Delete"]),
    (Wallpaper, "Wallpaper", "Set as wallpaper", &[]),
    (Close, "Close", "Close panel, stop slideshow, leave full screen, quit", &["Escape"]),
    (Quit, "Quit", "Quit", &["Ctrl+Q"]),
];

#[derive(Clone, PartialEq, Debug)]
struct Binding {
    ctrl: bool,
    shift: bool,
    alt: bool,
    key: String,
}

fn parse(text: &str) -> Option<Binding> {
    let mut b = Binding { ctrl: false, shift: false, alt: false, key: String::new() };
    for part in text.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => b.ctrl = true,
            "shift" => b.shift = true,
            "alt" => b.alt = true,
            "" => {}
            _ => b.key = normalize(part),
        }
    }
    (!b.key.is_empty()).then_some(b)
}

fn normalize(name: &str) -> String {
    let mut c = name.chars();
    match (c.next(), c.next()) {
        (Some(ch), None) => ch.to_uppercase().to_string(),
        _ => {
            let mut s = name.to_ascii_lowercase();
            if let Some(f) = s.get_mut(0..1) {
                f.make_ascii_uppercase();
            }
            // canonical spellings
            match s.as_str() {
                "Pageup" => "PageUp".into(),
                "Pagedown" => "PageDown".into(),
                "Esc" => "Escape".into(),
                "Return" => "Enter".into(),
                "Del" => "Delete".into(),
                _ => s,
            }
        }
    }
}

/// Name of a pressed key, as used in keys.conf
fn key_name(key: &Key) -> Option<String> {
    Some(match key {
        Key::Named(n) => match n {
            NamedKey::ArrowRight => "Right".into(),
            NamedKey::ArrowLeft => "Left".into(),
            NamedKey::ArrowUp => "Up".into(),
            NamedKey::ArrowDown => "Down".into(),
            NamedKey::Space => "Space".into(),
            other => normalize(&format!("{other:?}")),
        },
        Key::Character(c) => match c.as_str() {
            "+" => "Plus".into(),
            "-" => "Minus".into(),
            "=" => "Equal".into(),
            "," => "Comma".into(),
            "." => "Period".into(),
            "/" => "Slash".into(),
            s => s.to_uppercase(),
        },
        _ => return None,
    })
}

/// Shown name of a key in menus and help
fn label(b: &Binding) -> String {
    let mut s = String::new();
    if b.ctrl {
        s.push_str("Ctrl+");
    }
    if b.alt {
        s.push_str("Alt+");
    }
    if b.shift {
        s.push_str("Shift+");
    }
    s.push_str(match b.key.as_str() {
        "Plus" => "+",
        "Minus" => "-",
        "Equal" => "=",
        "Comma" => ",",
        "Period" => ".",
        "Escape" => "Esc",
        "Delete" => "Del",
        "Pageup" | "PageUp" => "Page Up",
        "Pagedown" | "PageDown" => "Page Down",
        k => k,
    });
    s
}

pub struct KeyMap {
    bindings: Vec<(Command, Vec<Binding>)>,
}

impl KeyMap {
    fn defaults() -> KeyMap {
        KeyMap { bindings: COMMANDS.iter().map(|(c, _, _, keys)| (*c, keys.iter().filter_map(|k| parse(k)).collect())).collect() }
    }

    pub fn path() -> PathBuf {
        crate::settings::Settings::path().with_file_name("keys.conf")
    }

    fn load() -> KeyMap {
        let mut map = KeyMap::defaults();
        let Ok(text) = std::fs::read_to_string(Self::path()) else { return map };
        for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#') && !l.is_empty()) {
            let Some((name, keys)) = line.split_once('=') else { continue };
            let Some((cmd, ..)) = COMMANDS.iter().find(|(_, n, ..)| n.eq_ignore_ascii_case(name.trim())) else { continue };
            let list: Vec<Binding> = keys.split(',').filter_map(parse).collect();
            if let Some(entry) = map.bindings.iter_mut().find(|(c, _)| c == cmd) {
                entry.1 = list;
            }
        }
        map
    }

    /// Writes keys.conf with all commands and their current keys (if it does not exist yet).
    pub fn write_file_if_missing() -> PathBuf {
        let path = Self::path();
        if !path.exists() {
            let mut out = String::from(
                "# Lumaris key bindings. One line per command: Command = Key, Key ...\n\
                 # Keys: letters, digits, F1..F12, Left, Right, Up, Down, PageUp, PageDown, Home, End, Space,\n\
                 # Enter, Backspace, Delete, Escape, Tab, Plus, Minus, Equal, Comma, Period. Modifiers: Ctrl+, Shift+, Alt+\n\
                 # An empty value removes all keys of a command. 1..9 always start a slideshow.\n\
                 # Restart Lumaris after editing this file.\n\n",
            );
            let current = map().read().unwrap();
            for (cmd, name, desc, _) in COMMANDS {
                let keys = current.keys(*cmd).join(", ");
                out.push_str(&format!("# {desc}\n{name} = {keys}\n"));
            }
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, out);
        }
        path
    }

    fn keys(&self, cmd: Command) -> Vec<String> {
        self.bindings
            .iter()
            .find(|(c, _)| *c == cmd)
            .map(|(_, b)| {
                b.iter()
                    .map(|b| {
                        let mut s = String::new();
                        if b.ctrl {
                            s.push_str("Ctrl+");
                        }
                        if b.alt {
                            s.push_str("Alt+");
                        }
                        if b.shift {
                            s.push_str("Shift+");
                        }
                        s + &b.key
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The command bound to the pressed key
    pub fn lookup(&self, key: &Key, mods: ModifiersState) -> Option<Command> {
        let name = key_name(key)?;
        // Shift is part of characters like "+" - it only counts for letters and named keys
        let letter = name.chars().count() == 1 && name.chars().all(|c| c.is_alphabetic());
        let named = name.chars().count() > 1 && !matches!(name.as_str(), "Plus" | "Minus" | "Equal" | "Comma" | "Period" | "Slash");
        let shift_counts = letter || named;
        self.bindings.iter().find_map(|(cmd, list)| {
            list.iter()
                .any(|b| {
                    b.key == name
                        && b.ctrl == mods.control_key()
                        && b.alt == mods.alt_key()
                        && (!shift_counts || b.shift == mods.shift_key())
                })
                .then_some(*cmd)
        })
    }

    /// Whether a key description like "Ctrl+G" matches the pressed key (user commands)
    pub fn matches(spec: &str, key: &Key, mods: ModifiersState) -> bool {
        let m = KeyMap { bindings: vec![(Command::Quit, parse(spec).into_iter().collect())] };
        m.lookup(key, mods).is_some()
    }

    /// Shown form of a key description ("Ctrl+Comma" -> "Ctrl+,")
    pub fn display(spec: &str) -> String {
        parse(spec).map(|b| label(&b)).unwrap_or_default()
    }

    /// First key of a command for menus ("" if none)
    pub fn shortcut(&self, cmd: Command) -> String {
        self.bindings.iter().find(|(c, _)| *c == cmd).and_then(|(_, b)| b.first()).map(label).unwrap_or_default()
    }

    /// All keys of a command for the help
    pub fn shortcuts(&self, cmd: Command) -> String {
        self.bindings.iter().find(|(c, _)| *c == cmd).map(|(_, b)| b.iter().map(label).collect::<Vec<_>>().join(", ")).unwrap_or_default()
    }
}

fn map() -> &'static RwLock<KeyMap> {
    static MAP: OnceLock<RwLock<KeyMap>> = OnceLock::new();
    MAP.get_or_init(|| RwLock::new(KeyMap::load()))
}

pub fn current() -> std::sync::RwLockReadGuard<'static, KeyMap> {
    map().read().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bindings() {
        let m = KeyMap::defaults();
        let none = ModifiersState::empty();
        assert_eq!(m.lookup(&Key::Named(NamedKey::ArrowRight), none), Some(Next));
        assert_eq!(m.lookup(&Key::Character("r".into()), none), Some(RotateRight));
        assert_eq!(m.lookup(&Key::Character("R".into()), ModifiersState::SHIFT), Some(RotateLeft));
        assert_eq!(m.lookup(&Key::Character("+".into()), ModifiersState::SHIFT), Some(ZoomIn));
        assert_eq!(m.lookup(&Key::Character("c".into()), ModifiersState::CONTROL), Some(CopyImage));
        assert_eq!(m.lookup(&Key::Character(",".into()), ModifiersState::CONTROL), Some(Settings));
        assert_eq!(m.shortcut(CopyPath), "Ctrl+Shift+C");
    }
}
