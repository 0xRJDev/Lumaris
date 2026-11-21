// User commands: own entries for the menu and keys, run with the current image.
// ~/.config/lumaris/commands.conf, one per line:  Name | Key | Command
// %f = the image file, %d = its folder (both quoted for the shell).
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct UserCommand {
    pub name: String,
    pub key: String,
    command: String,
}

pub fn path() -> PathBuf {
    crate::settings::Settings::path().with_file_name("commands.conf")
}

const TEMPLATE: &str = "# Lumaris user commands: they appear in the right-click menu (Commands) and can have a key.\n\
# One per line:  Name | Key | Command      (the key may be empty)\n\
# %f = the image file, %d = its folder. The command runs in a shell. Restart Lumaris after editing.\n\
# Examples:\n\
# Edit in GIMP | Ctrl+G | gimp %f\n\
# Copy to ~/Pictures/Keep | Shift+K | mkdir -p ~/Pictures/Keep && cp %f ~/Pictures/Keep/\n";

pub fn write_file_if_missing() -> PathBuf {
    let p = path();
    if !p.exists() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&p, TEMPLATE);
    }
    p
}

pub fn load() -> Vec<UserCommand> {
    let Ok(text) = std::fs::read_to_string(path()) else { return Vec::new() };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .filter_map(|l| {
            let mut p = l.splitn(3, '|').map(str::trim);
            let (name, key, command) = (p.next()?, p.next()?, p.next()?);
            (!name.is_empty() && !command.is_empty()).then(|| UserCommand { name: name.into(), key: key.into(), command: command.into() })
        })
        .collect()
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

impl UserCommand {
    pub fn run(&self, file: &Path) {
        let f = file.to_string_lossy();
        let d = file.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        let cmd = self.command.replace("%f", &quote(&f)).replace("%d", &quote(&d));
        crate::apps::spawn_detached(Command::new("sh").arg("-c").arg(cmd).current_dir(file.parent().unwrap_or(Path::new("/"))));
    }
}
