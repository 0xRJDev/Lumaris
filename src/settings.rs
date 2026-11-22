// Settings: defined once here (section, label, type, default, explanation), stored in
// ~/.config/lumaris/lumaris.conf and edited in the settings window.
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Value {
    Bool(bool),
    Choice(usize),
    Number(f64),
}

pub enum Kind {
    Bool,
    Choice(&'static [&'static str]),
    Number { min: f64, max: f64, step: f64, unit: &'static str },
}

pub struct Def {
    pub section: &'static str,
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub default: Value,
    /// takes effect after restarting Lumaris
    pub restart: bool,
}

pub const SECTIONS: &[&str] = &["Window", "Background", "Zoom", "Navigation", "Slideshow", "Formats", "Advanced"];

pub const ASPECTS: &[&str] = &["4:3", "16:10", "16:9", "21:9", "32:9", "No limit"];
pub const BACKGROUNDS: &[&str] = &["Blurred desktop", "See-through desktop", "Solid colour", "Checkerboard"];
pub const COLOURS: &[&str] = &["Black", "Dark grey", "Grey", "White"];
pub const FIT_MODES: &[&str] = &["Shrink large images", "Also enlarge small images"];
pub const WHEEL: &[&str] = &["Zoom", "Next / previous image"];
pub const SORT: &[&str] = &["Name", "Date modified", "File size", "Random"];
pub const TRANSITIONS: &[&str] = &["None", "Fade"];
pub const RAW: &[&str] = &["Embedded preview (fast)", "Develop RAW (best quality)"];

use Kind::*;
use Value::{Bool as B, Choice as C, Number as N};

macro_rules! def {
    ($section:expr, $key:expr, $label:expr, $kind:expr, $default:expr, $help:expr) => {
        Def { section: $section, key: $key, label: $label, help: $help, kind: $kind, default: $default, restart: false }
    };
    ($section:expr, $key:expr, $label:expr, $kind:expr, $default:expr, $help:expr, restart) => {
        Def { section: $section, key: $key, label: $label, help: $help, kind: $kind, default: $default, restart: true }
    };
}

pub const DEFS: &[Def] = &[
    def!(
        "Window",
        "MaxWindowSize",
        "Opening size (part of the screen)",
        Number { min: 30.0, max: 100.0, step: 5.0, unit: "%" },
        N(80.0),
        "Largest part of the screen a new window may take; the image is fitted into it"
    ),
    def!(
        "Window",
        "MinWindowSize",
        "Smallest opening size",
        Number { min: 0.0, max: 50.0, step: 5.0, unit: "%" },
        N(10.0),
        "Small images open enlarged to at least this part of the screen height (zooming out still works)"
    ),
    def!(
        "Window",
        "MaxWindowAspect",
        "Widest window shape",
        Choice(ASPECTS),
        C(2),
        "Widest window aspect ratio, unless the image itself is wider"
    ),
    def!(
        "Window",
        "WindowFollowsZoom",
        "Window follows the zoom",
        Bool,
        B(true),
        "While zooming the window grows and shrinks with the image, up to the screen size"
    ),
    def!("Window", "StartFullScreen", "Open in full screen", Bool, B(false), "Start in full screen mode"),
    def!("Window", "KeepAbove", "Keep window above others", Bool, B(false), "Keep the window above other windows (KWin)"),
    def!(
        "Window",
        "NavigationPanel",
        "Navigation panel",
        Bool,
        B(true),
        "Buttons and a zoom map at the bottom of the window, shown while the mouse moves"
    ),
    def!(
        "Window",
        "SingleInstance",
        "Single window",
        Bool,
        B(false),
        "Images opened while Lumaris runs are shown in the running window",
        restart
    ),
    def!("Background", "BackgroundMode", "Background", Choice(BACKGROUNDS), C(0), "What is behind the image and transparent image parts"),
    def!(
        "Background",
        "BackgroundOpacity",
        "Background opacity",
        Number { min: 0.0, max: 100.0, step: 10.0, unit: "%" },
        N(0.0),
        "With the desktop backgrounds: how much the background colour covers the desktop"
    ),
    def!("Background", "BackgroundColour", "Background colour", Choice(COLOURS), C(0), "Colour around the image"),
    def!("Zoom", "FitMode", "Fit to window", Choice(FIT_MODES), C(0), "Whether small images are enlarged to fill the window"),
    def!("Zoom", "SmoothZoom", "Smooth zoom animation", Bool, B(true), "Glide to the new zoom instead of jumping"),
    def!("Zoom", "WheelAction", "Mouse wheel", Choice(WHEEL), C(0), "What the mouse wheel does (Ctrl+wheel does the other)"),
    def!(
        "Zoom",
        "ZoomStep",
        "Zoom step per wheel notch",
        Number { min: 5.0, max: 50.0, step: 5.0, unit: "%" },
        N(20.0),
        "How much one wheel notch or +/- zooms"
    ),
    def!(
        "Zoom",
        "PixelArt",
        "Sharp pixels for pixel art",
        Bool,
        B(true),
        "Enlarge small images with square pixels instead of smoothing them"
    ),
    def!(
        "Zoom",
        "PixelArtMaxSize",
        "Pixel art: largest image size",
        Number { min: 32.0, max: 1024.0, step: 32.0, unit: "px" },
        N(256.0),
        "Images up to this width and height count as pixel art"
    ),
    def!("Navigation", "LoopFolder", "Loop through folder", Bool, B(true), "After the last image comes the first one"),
    def!(
        "Navigation",
        "IgnoreHidden",
        "Skip hidden images",
        Bool,
        B(true),
        "Leave out images whose name starts with a dot (.image.png), unless you open one directly"
    ),
    def!(
        "Navigation",
        "WatchFolder",
        "Follow changes in the folder",
        Bool,
        B(true),
        "Images added to or removed from the folder (by other programs) appear and disappear while browsing"
    ),
    def!("Navigation", "SortOrder", "Sort order", Choice(SORT), C(0), "Order of the images of a folder"),
    def!("Navigation", "SortReverse", "Reverse sort order", Bool, B(false), "Sort in reverse order"),
    def!(
        "Navigation",
        "KeepZoom",
        "Keep zoom on next image",
        Bool,
        B(false),
        "Keep the zoom when going to another image instead of fitting it"
    ),
    def!(
        "Slideshow",
        "SlideshowInterval",
        "Default interval",
        Number { min: 1.0, max: 60.0, step: 1.0, unit: "s" },
        N(3.0),
        "Seconds per image when the slideshow is started from the menu"
    ),
    def!("Slideshow", "SlideshowStopAtEnd", "Stop at the last image", Bool, B(false), "Stop instead of starting over"),
    def!("Slideshow", "SlideshowFullScreen", "Start in full screen", Bool, B(false), "Switch to full screen when a slideshow starts"),
    def!("Slideshow", "Transition", "Transition", Choice(TRANSITIONS), C(1), "How the slideshow changes images"),
    def!("Formats", "PlayAnimations", "Play animations", Bool, B(true), "Play animated GIF, WebP and PNG files"),
    def!("Formats", "RawMode", "Camera RAW files", Choice(RAW), C(0), "Show the camera's embedded preview, or develop the RAW data (slow)"),
    def!(
        "Formats",
        "ColourProfiles",
        "Use embedded colour profiles",
        Bool,
        B(true),
        "Convert images with an embedded colour profile (ICC) to sRGB"
    ),
    def!(
        "Advanced",
        "PreferDedicatedGpu",
        "Prefer the dedicated GPU",
        Bool,
        B(true),
        "Use the more powerful GPU when there are several",
        restart
    ),
    def!(
        "Advanced",
        "Prefetch",
        "Images decoded in advance",
        Number { min: 0.0, max: 3.0, step: 1.0, unit: "" },
        N(1.0),
        "How many images before and after the current one are decoded in advance"
    ),
    def!(
        "Advanced",
        "MemoryLimit",
        "Memory for decoded images",
        Number { min: 256.0, max: 16384.0, step: 256.0, unit: "MB" },
        N(1024.0),
        "Images decoded in advance are kept only while they fit in this much memory (the current image always stays)"
    ),
];

#[derive(Clone)]
pub struct Settings {
    values: Vec<Value>,
}

fn index(key: &str) -> usize {
    DEFS.iter().position(|d| d.key == key).unwrap_or_else(|| panic!("unknown setting {key}"))
}

impl Settings {
    pub fn defaults() -> Settings {
        Settings { values: DEFS.iter().map(|d| d.default).collect() }
    }
    pub fn get(&self, i: usize) -> Value {
        self.values[i]
    }
    pub fn set(&mut self, i: usize, v: Value) {
        self.values[i] = v;
    }
    pub fn bool(&self, key: &str) -> bool {
        matches!(self.values[index(key)], Value::Bool(true))
    }
    pub fn choice(&self, key: &str) -> usize {
        match self.values[index(key)] {
            Value::Choice(c) => c,
            _ => 0,
        }
    }
    pub fn number(&self, key: &str) -> f64 {
        match self.values[index(key)] {
            Value::Number(n) => n,
            _ => 0.0,
        }
    }

    pub fn path() -> PathBuf {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let cfg = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or(home.join(".config"));
        cfg.join("lumaris").join("lumaris.conf")
    }

    pub fn load() -> Settings {
        let mut s = Settings::defaults();
        let Ok(text) = std::fs::read_to_string(Self::path()) else { return s };
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            let Some(i) = DEFS.iter().position(|d| d.key.eq_ignore_ascii_case(k)) else { continue };
            let value = match &DEFS[i].kind {
                Kind::Bool => match v.to_ascii_lowercase().as_str() {
                    "true" | "yes" | "on" | "1" => Some(Value::Bool(true)),
                    "false" | "no" | "off" | "0" => Some(Value::Bool(false)),
                    _ => None,
                },
                Kind::Choice(options) => options.iter().position(|o| o.eq_ignore_ascii_case(v)).map(Value::Choice),
                Kind::Number { min, max, .. } => v
                    .trim_end_matches(|c: char| c.is_alphabetic() || c == '%')
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .map(|n| Value::Number(n.clamp(*min, *max))),
            };
            if let Some(value) = value {
                s.values[i] = value;
            }
        }
        s
    }

    /// Writes the complete, commented settings file.
    pub fn save(&self) -> std::io::Result<()> {
        let mut out = String::from(
            "# Lumaris settings\n#\n# Changed by the settings window (right-click menu > Settings, or Ctrl+,). You can also edit\n# this file; restart Lumaris afterwards. Lines starting with # are explanations.\n",
        );
        for section in SECTIONS {
            out.push_str(&format!("\n[{section}]\n"));
            for (i, d) in DEFS.iter().enumerate().filter(|(_, d)| d.section == *section) {
                let options = match &d.kind {
                    Kind::Choice(o) => format!(" ({})", o.join(" | ")),
                    Kind::Number { min, max, unit, .. } => {
                        format!(" ({min}..{max}{})", if unit.is_empty() { String::new() } else { format!(" {unit}") })
                    }
                    Kind::Bool => " (true | false)".into(),
                };
                let value = match (self.values[i], &d.kind) {
                    (Value::Bool(b), _) => b.to_string(),
                    (Value::Choice(c), Kind::Choice(o)) => o.get(c).copied().unwrap_or("").to_string(),
                    (Value::Number(n), _) => format!("{n}"),
                    _ => String::new(),
                };
                out.push_str(&format!(
                    "# {}{}{}\n{}={}\n",
                    d.help,
                    options,
                    if d.restart { " - after a restart" } else { "" },
                    d.key,
                    value
                ));
            }
        }
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, out)
    }
}

/// The current settings, shared with the decoder threads.
pub fn current() -> std::sync::RwLockReadGuard<'static, Settings> {
    lock().read().unwrap()
}

pub fn replace(s: Settings) {
    *lock().write().unwrap() = s;
}

fn lock() -> &'static RwLock<Settings> {
    static S: OnceLock<RwLock<Settings>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(Settings::load()))
}

pub fn aspect_ratio(choice: usize) -> f64 {
    match choice {
        0 => 4.0 / 3.0,
        1 => 16.0 / 10.0,
        2 => 16.0 / 9.0,
        3 => 21.0 / 9.0,
        4 => 32.0 / 9.0,
        _ => f64::INFINITY,
    }
}

pub fn colour(choice: usize) -> [f64; 3] {
    match choice {
        1 => [0.12, 0.12, 0.12],
        2 => [0.4, 0.4, 0.4],
        3 => [1.0, 1.0, 1.0],
        _ => [0.0, 0.0, 0.0],
    }
}
