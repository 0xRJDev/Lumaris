// "Open image" dialog: the desktop's own file dialog through the xdg-desktop-portal
// (org.freedesktop.portal.FileChooser), kdialog or zenity as fallbacks.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use zbus::zvariant::{OwnedValue, Value};

/// Decodes a file:// URI
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.find('/').map(|i| &rest[i..])?; // skip the host part
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

fn portal(folder: Option<&Path>) -> Option<Option<PathBuf>> {
    let conn = zbus::blocking::Connection::session().ok()?;
    let token = format!("lumaris{}", std::process::id());
    let sender = conn.unique_name()?.trim_start_matches(':').replace('.', "_");
    let request = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let proxy =
        zbus::blocking::Proxy::new(&conn, "org.freedesktop.portal.Desktop", request.as_str(), "org.freedesktop.portal.Request").ok()?;
    let mut responses = proxy.receive_signal("Response").ok()?;
    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("multiple", Value::from(false));
    let filters = vec![("Images", vec![(1u32, "image/*")]), ("All files", vec![(0u32, "*")])];
    options.insert("filters", Value::from(filters));
    if let Some(f) = folder {
        let mut bytes = f.as_os_str().as_encoded_bytes().to_vec();
        bytes.push(0);
        options.insert("current_folder", Value::from(bytes));
    }
    conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.FileChooser"),
        "OpenFile",
        &("", "Open image", options),
    )
    .ok()?;
    let msg = responses.next()?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize().ok()?;
    if code != 0 {
        return Some(None); // cancelled
    }
    let uris: Vec<String> = results.get("uris").and_then(|v| v.try_clone().ok()).and_then(|v| Vec::<String>::try_from(v).ok())?;
    Some(uris.first().and_then(|u| uri_to_path(u)))
}

fn fallback(folder: Option<&Path>) -> Option<PathBuf> {
    let dir = folder.map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| std::env::var("HOME").unwrap_or_default());
    let run = |c: &mut Command| {
        c.output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    run(Command::new("kdialog").args(["--title", "Open image", "--getopenfilename", &dir, "image/*"]))
        .or_else(|| run(Command::new("zenity").args(["--file-selection", "--title=Open image", &format!("--filename={dir}/")])))
        .map(PathBuf::from)
}

/// Shows the dialog and waits for it. None: cancelled (or no dialog available).
pub fn pick_image(folder: Option<&Path>) -> Option<PathBuf> {
    match portal(folder) {
        Some(result) => result,
        None => fallback(folder),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uris() {
        assert_eq!(uri_to_path("file:///home/a/b%20c.png"), Some(PathBuf::from("/home/a/b c.png")));
        assert_eq!(uri_to_path("file://host/x/%C3%A9.jpg"), Some(PathBuf::from("/x/é.jpg")));
    }
}
