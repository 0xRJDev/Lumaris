// Watches the folder being browsed: when images are added, removed or renamed there (the
// folder's modification time changes), the main loop is told to list it again.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

const INTERVAL: Duration = Duration::from_secs(1);

pub struct Watch {
    folder: Arc<Mutex<Option<PathBuf>>>,
}

impl Watch {
    pub fn start(proxy: EventLoopProxy<UserEvent>) -> Watch {
        let folder: Arc<Mutex<Option<PathBuf>>> = Arc::default();
        let watched = folder.clone();
        std::thread::spawn(move || {
            let mut last: Option<(PathBuf, SystemTime)> = None;
            loop {
                std::thread::sleep(INTERVAL);
                let Some(dir) = watched.lock().unwrap().clone() else {
                    last = None;
                    continue;
                };
                let Ok(modified) = std::fs::metadata(&dir).and_then(|m| m.modified()) else { continue };
                let changed = last.as_ref().is_some_and(|(d, t)| *d == dir && *t != modified);
                last = Some((dir.clone(), modified));
                if changed && proxy.send_event(UserEvent::FolderChanged(dir)).is_err() {
                    break;
                }
            }
        });
        Watch { folder }
    }

    /// The folder to watch (None: none)
    pub fn set(&self, folder: Option<PathBuf>) {
        *self.folder.lock().unwrap() = folder;
    }
}
