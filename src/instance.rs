// Single instance mode (setting): a second `lumaris <file>` hands the file to the running
// Lumaris over D-Bus and exits.
use std::path::Path;

use winit::event_loop::EventLoopProxy;
use zbus::fdo::{RequestNameFlags, RequestNameReply};

use crate::UserEvent;

const NAME: &str = "org.lumaris.Viewer";
const OBJECT: &str = "/org/lumaris/Viewer";

struct Remote {
    proxy: EventLoopProxy<UserEvent>,
}

#[zbus::interface(name = "org.lumaris.Viewer")]
impl Remote {
    #[zbus(name = "Open")]
    fn open(&self, path: String) {
        let _ = self.proxy.send_event(UserEvent::Open(path.into()));
    }
}

pub enum Instance {
    /// this is the running instance (keeps the D-Bus name while alive)
    First(#[allow(dead_code)] zbus::blocking::Connection),
    /// the file was handed to the running instance
    Forwarded,
}

pub fn claim(file: &Path, proxy: EventLoopProxy<UserEvent>) -> Option<Instance> {
    let conn = zbus::blocking::Connection::session().ok()?;
    conn.object_server().at(OBJECT, Remote { proxy }).ok()?;
    // never take the name from a running Lumaris
    match conn.request_name_with_flags(NAME, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => Some(Instance::First(conn)),
        Err(e) if !matches!(e, zbus::Error::NameTaken) => None,
        _ => {
            // another Lumaris runs
            let abs = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
            conn.call_method(Some(NAME), OBJECT, Some(NAME), "Open", &(abs.to_string_lossy().as_ref(),)).ok()?;
            Some(Instance::Forwarded)
        }
    }
}
