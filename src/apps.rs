// "Open image with": the applications associated with a file type, found the freedesktop way
// (shared-mime-info globs, mimeapps.list, desktop entries). The default application (e.g. the
// image editor chosen in System Settings) comes first.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct DesktopApp {
    pub id: String,
    pub name: String,
    exec: String,
    mime_types: Vec<String>,
    preference: i32,
}

struct Database {
    apps: HashMap<String, DesktopApp>,
    /// "*.ext" (lowercase) -> mime type
    globs: HashMap<String, String>,
    defaults: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
    removed: HashMap<String, HashSet<String>>,
}

fn env_path(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        home.join(fallback)
    })
}

fn env_dirs(var: &str, fallback: &str) -> Vec<PathBuf> {
    std::env::var(var).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| fallback.to_string()).split(':').map(PathBuf::from).collect()
}

fn data_dirs() -> Vec<PathBuf> {
    let mut v = vec![env_path("XDG_DATA_HOME", ".local/share")];
    v.extend(env_dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share"));
    v
}

/// Parses an INI style file into sections of key/value pairs.
fn parse_ini(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_string();
        } else if let Some((k, v)) = line.split_once('=') {
            out.entry(section.clone()).or_default().entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
        }
    }
    out
}

fn list(v: &str) -> Vec<String> {
    v.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

fn load() -> Database {
    let dirs = data_dirs();
    // desktop entries: earlier directories take precedence
    let mut apps = HashMap::new();
    for dir in dirs.iter().rev() {
        let base = dir.join("applications");
        collect_desktop_files(&base, &base, &mut apps);
    }
    // mime type globs
    let mut globs = HashMap::new();
    for dir in dirs.iter().rev() {
        if let Ok(text) = std::fs::read_to_string(dir.join("mime/globs2")) {
            let mut entries: Vec<(i32, String, String)> = text
                .lines()
                .filter(|l| !l.starts_with('#'))
                .filter_map(|l| {
                    let mut p = l.splitn(4, ':');
                    let w = p.next()?.parse().ok()?;
                    Some((w, p.next()?.to_string(), p.next()?.to_lowercase()))
                })
                .collect();
            entries.sort_by_key(|e| e.0); // higher weight wins (inserted last)
            for (_, mime, glob) in entries {
                globs.insert(glob, mime);
            }
        }
    }
    // mimeapps.list files, most important first
    let desktops: Vec<String> =
        std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().split(':').filter(|s| !s.is_empty()).map(|s| s.to_lowercase()).collect();
    let mut config_dirs = vec![env_path("XDG_CONFIG_HOME", ".config")];
    config_dirs.extend(env_dirs("XDG_CONFIG_DIRS", "/etc/xdg"));
    let mut files = Vec::new();
    for d in config_dirs.iter().cloned().chain(dirs.iter().map(|d| d.join("applications"))) {
        for de in &desktops {
            files.push(d.join(format!("{de}-mimeapps.list")));
        }
        files.push(d.join("mimeapps.list"));
    }
    let (mut defaults, mut added, mut removed) =
        (HashMap::new(), HashMap::<String, Vec<String>>::new(), HashMap::<String, HashSet<String>>::new());
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        let ini = parse_ini(&text);
        if let Some(s) = ini.get("Default Applications") {
            for (mime, ids) in s {
                defaults.entry(mime.clone()).or_insert_with(|| list(ids));
            }
        }
        if let Some(s) = ini.get("Added Associations") {
            for (mime, ids) in s {
                let e = added.entry(mime.clone()).or_default();
                for id in list(ids) {
                    if !e.contains(&id) {
                        e.push(id);
                    }
                }
            }
        }
        if let Some(s) = ini.get("Removed Associations") {
            for (mime, ids) in s {
                removed.entry(mime.clone()).or_default().extend(list(ids));
            }
        }
    }
    Database { apps, globs, defaults, added, removed }
}

fn collect_desktop_files(base: &Path, dir: &Path, apps: &mut HashMap<String, DesktopApp>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_desktop_files(base, &p, apps);
            continue;
        }
        if p.extension().is_none_or(|x| x != "desktop") {
            continue;
        }
        let id = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().replace('/', "-");
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let ini = parse_ini(&text);
        let Some(entry) = ini.get("Desktop Entry") else { continue };
        let hidden = entry.get("Hidden").is_some_and(|v| v == "true") || entry.get("Type").is_some_and(|t| t != "Application");
        if hidden {
            apps.remove(&id); // a user entry can hide a system one
            continue;
        }
        let (Some(name), Some(exec)) = (entry.get("Name"), entry.get("Exec")) else { continue };
        apps.insert(
            id.clone(),
            DesktopApp {
                id,
                name: name.clone(),
                exec: exec.clone(),
                mime_types: entry.get("MimeType").map(|m| list(m)).unwrap_or_default(),
                preference: entry.get("InitialPreference").and_then(|v| v.parse().ok()).unwrap_or(0),
            },
        );
    }
}

fn database() -> &'static Database {
    static DB: OnceLock<Database> = OnceLock::new();
    DB.get_or_init(load)
}

/// Loads the application database in the background (so the menu opens instantly).
pub fn warm_up() {
    std::thread::spawn(|| {
        database();
    });
}

pub fn mime_type(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    database().globs.get(&format!("*.{ext}")).cloned()
}

/// Applications that open this file, the default first. Lumaris itself is left out.
pub fn for_file(path: &Path) -> Vec<DesktopApp> {
    let db = database();
    let Some(mime) = mime_type(path) else { return Vec::new() };
    let removed = db.removed.get(&mime);
    let mut ids: Vec<String> = Vec::new();
    let push = |id: &String, ids: &mut Vec<String>| {
        if !ids.contains(id) && db.apps.contains_key(id) && removed.is_none_or(|r| !r.contains(id)) {
            ids.push(id.clone());
        }
    };
    for id in db.defaults.get(&mime).into_iter().flatten() {
        push(id, &mut ids);
    }
    for id in db.added.get(&mime).into_iter().flatten() {
        push(id, &mut ids);
    }
    let mut rest: Vec<&DesktopApp> = db.apps.values().filter(|a| a.mime_types.contains(&mime)).collect();
    rest.sort_by(|a, b| b.preference.cmp(&a.preference).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    for a in rest {
        push(&a.id, &mut ids);
    }
    ids.into_iter().filter(|id| id != "lumaris.desktop").filter_map(|id| db.apps.get(&id).cloned()).collect()
}

/// Splits an Exec value into arguments (desktop entry quoting rules).
fn split_exec(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut has) = (false, false);
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                has = true;
            }
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ' ' | '\t' if !quoted => {
                if has || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            _ => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        args.push(cur);
    }
    args
}

pub fn file_uri(path: &Path) -> String {
    let mut s = String::from("file://");
    for b in path.as_os_str().as_encoded_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => s.push(*b as char),
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

/// Starts the application with the file.
pub fn launch(app: &DesktopApp, file: &Path) {
    let f = file.to_string_lossy().to_string();
    let mut args = Vec::new();
    let mut added = false;
    for a in split_exec(&app.exec) {
        match a.as_str() {
            "%f" | "%F" => {
                args.push(f.clone());
                added = true;
            }
            "%u" | "%U" => {
                args.push(file_uri(file));
                added = true;
            }
            "%c" => args.push(app.name.clone()),
            "%i" | "%k" => {}
            s if s.len() == 2 && s.starts_with('%') => {} // deprecated codes
            s => args.push(s.replace("%%", "%")),
        }
    }
    if !added {
        args.push(f);
    }
    if args.is_empty() {
        return;
    }
    let program = args.remove(0);
    spawn_detached(Command::new(program).args(args).current_dir(file.parent().unwrap_or(Path::new("/"))));
}

/// Runs a program without waiting for it (reaped on a background thread).
pub fn spawn_detached(cmd: &mut Command) {
    match cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => eprintln!("lumaris: cannot start {:?}: {e}", cmd.get_program()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exec_split() {
        assert_eq!(split_exec(r#"gimp-2.10 %U"#), ["gimp-2.10", "%U"]);
        assert_eq!(split_exec(r#""/opt/My App/run" --x "a b" %f"#), ["/opt/My App/run", "--x", "a b", "%f"]);
    }
}
