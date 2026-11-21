// Moving files to the trash (freedesktop.org trash specification: ~/.local/share/Trash).
// Files on other drives go through `gio trash` or KDE's `kioclient`, which know their trash
// folders.
use std::path::{Path, PathBuf};
use std::process::Command;

fn trash_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or(home.join(".local/share")).join("Trash")
}

fn encode_path(path: &Path) -> String {
    let mut s = String::new();
    for b in path.as_os_str().as_encoded_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => s.push(*b as char),
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

/// Moves the file to the trash. Returns an error message on failure.
pub fn trash(path: &Path) -> Result<(), String> {
    let dir = trash_dir();
    let (files, info) = (dir.join("files"), dir.join("info"));
    std::fs::create_dir_all(&files).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&info).map_err(|e| e.to_string())?;
    let name = path.file_name().ok_or("no file name")?.to_string_lossy().into_owned();
    // a free name in the trash
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    let mut n = 1;
    let mut target = name.clone();
    let info_file = loop {
        let candidate = info.join(format!("{target}.trashinfo"));
        // create_new: reserves the name (spec: the info file is created first)
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(_) => break candidate,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                n += 1;
                target = format!("{stem} ({n}){ext}");
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let date = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    std::fs::write(&info_file, format!("[Trash Info]\nPath={}\nDeletionDate={date}\n", encode_path(&abs))).map_err(|e| e.to_string())?;
    match std::fs::rename(path, files.join(&target)) {
        Ok(()) => Ok(()),
        Err(_) => {
            // other drive: let the desktop tools find the right trash folder
            let _ = std::fs::remove_file(&info_file);
            let ok = |c: &mut Command| c.status().is_ok_and(|s| s.success());
            if ok(Command::new("gio").arg("trash").arg(path)) || ok(Command::new("kioclient").args(["move"]).arg(path).arg("trash:/")) {
                Ok(())
            } else {
                Err("this drive has no trash".into())
            }
        }
    }
}
