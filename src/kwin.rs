// Window placement on KWin (Wayland clients cannot position their windows).
//
// A small KWin script is loaded over D-Bus for Lumaris's lifetime (nothing is installed, it is
// unloaded at exit). It centres Lumaris's window when KWin adds it - before the first frame, so
// the window does not visibly move - and keeps the window centre in place whenever Lumaris
// resizes the window (new image, zoom), in the same frame. It also reports the window frame
// (decoration) size and the usable screen area, so Lumaris can keep the window on screen.
use std::sync::{Arc, Mutex};

use zbus::blocking::Connection;

/// Window frame and screen area as reported by KWin (logical pixels).
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// Decoration size: left, top, right, bottom.
    pub margins: [i32; 4],
    /// Usable area of the window's screen (without panels).
    pub area: (i32, i32),
}

struct Receiver {
    frame: Arc<Mutex<Option<Frame>>>,
}

#[zbus::interface(name = "org.lumaris.Placement")]
impl Receiver {
    /// "left,top,right,bottom,areaWidth,areaHeight"
    #[zbus(name = "frame")]
    fn frame(&self, info: String) {
        let v: Vec<i32> = info.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if v.len() == 6 {
            *self.frame.lock().unwrap() = Some(Frame { margins: [v[0], v[1], v[2], v[3]], area: (v[4], v[5]) });
        }
    }
}

pub struct KWin {
    conn: Connection,
    name: String,
    frame: Arc<Mutex<Option<Frame>>>,
}

impl KWin {
    /// Loads the script. None when not running on KWin (or D-Bus is unavailable).
    pub fn start() -> Option<KWin> {
        std::env::var_os("WAYLAND_DISPLAY")?;
        let frame = Arc::new(Mutex::new(None));
        let conn = zbus::blocking::connection::Builder::session()
            .ok()?
            .serve_at("/lumaris/placement", Receiver { frame: frame.clone() })
            .ok()?
            .build()
            .ok()?;
        let has_kwin: bool = conn
            .call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "NameHasOwner", &("org.kde.KWin",))
            .ok()?
            .body()
            .deserialize()
            .ok()?;
        if !has_kwin {
            return None;
        }
        let service = conn.unique_name()?.to_string();
        let pid = std::process::id();
        let name = format!("lumaris-keepcenter-{pid}");
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let path = dir.join(format!("{name}.js"));
        let script = SCRIPT.replace("%PID%", &pid.to_string()).replace("%SERVICE%", &service);
        std::fs::write(&path, script).ok()?;
        let loaded = (|| {
            let id: i32 = conn
                .call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), "loadScript", &(path.to_str()?, name.as_str()))
                .ok()?
                .body()
                .deserialize()
                .ok()?;
            if id < 0 {
                return None;
            }
            conn.call_method(Some("org.kde.KWin"), format!("/Scripting/Script{id}").as_str(), Some("org.kde.kwin.Script"), "run", &())
                .ok()?;
            Some(())
        })();
        let _ = std::fs::remove_file(&path);
        loaded?;
        Some(KWin { conn, name, frame })
    }

    /// Keeps Lumaris's window above other windows (or not).
    pub fn set_keep_above(&self, on: bool) {
        let pid = std::process::id();
        let js = format!(
            "(function() {{ var l = workspace.windowList(); for (var i = 0; i < l.length; i++) {{ if (l[i].pid == {pid} && l[i].normalWindow) l[i].keepAbove = {on}; }} }})();"
        );
        let name = format!("lumaris-keepabove-{pid}");
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let path = dir.join(format!("{name}.js"));
        if std::fs::write(&path, js).is_err() {
            return;
        }
        if let Ok(reply) = self.conn.call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), "loadScript", &(path.to_str().unwrap_or(""), name.as_str())) {
            if let Ok(id) = reply.body().deserialize::<i32>() {
                if id >= 0 {
                    let _ = self.conn.call_method(Some("org.kde.KWin"), format!("/Scripting/Script{id}").as_str(), Some("org.kde.kwin.Script"), "run", &());
                }
            }
        }
        let _ = self.conn.call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), "unloadScript", &(name.as_str(),));
        let _ = std::fs::remove_file(&path);
    }

    pub fn frame(&self) -> Option<Frame> {
        *self.frame.lock().unwrap()
    }
}

impl Drop for KWin {
    fn drop(&mut self) {
        let _ = self.conn.call_method(Some("org.kde.KWin"), "/Scripting", Some("org.kde.kwin.Scripting"), "unloadScript", &(self.name.as_str(),));
    }
}

const SCRIPT: &str = r#"(function() {
  var PID = %PID%; var SERVICE = '%SERVICE%';
  function attach(w, fresh) {
    if (!w || w.pid != PID || !w.normalWindow || w.transient) return;
    var own = false, lastSize = null, cx = 0, cy = 0, info = '';
    function report() {
      var g = w.frameGeometry, c = w.clientGeometry, a = workspace.clientArea(KWin.PlacementArea, w);
      var m = [c.x - g.x, c.y - g.y, g.x + g.width - c.x - c.width, g.y + g.height - c.y - c.height, a.width, a.height]
        .map(function(v) { return Math.round(v); }).join(',');
      if (m != info) { info = m; callDBus(SERVICE, '/lumaris/placement', 'org.lumaris.Placement', 'frame', m); }
    }
    function remember() { var g = w.frameGeometry; cx = g.x + g.width / 2; cy = g.y + g.height / 2; lastSize = { width: g.width, height: g.height }; }
    if (fresh) {
      var a0 = workspace.clientArea(KWin.PlacementArea, w), g0 = w.frameGeometry;
      w.frameGeometry = { x: Math.round(a0.x + (a0.width - g0.width) / 2), y: Math.round(a0.y + Math.max(0, (a0.height - g0.height) / 2)), width: g0.width, height: g0.height };
    }
    remember(); report();
    w.frameGeometryChanged.connect(function() {
      var g = w.frameGeometry;
      report();
      if (own) { own = false; return; }
      if (w.fullScreen || w.move || w.resize) { remember(); return; }
      var sized = Math.abs(g.width - lastSize.width) >= 0.5 || Math.abs(g.height - lastSize.height) >= 0.5;
      if (!sized) { remember(); return; }
      lastSize = { width: g.width, height: g.height };
      var a = workspace.clientArea(KWin.PlacementArea, w);
      var x = Math.round(cx - g.width / 2), y = Math.round(cy - g.height / 2);
      x = Math.max(a.x, Math.min(a.x + a.width - g.width, x));
      y = Math.max(a.y, Math.min(a.y + a.height - g.height, y));
      if (Math.abs(x - g.x) >= 0.5 || Math.abs(y - g.y) >= 0.5) { own = true; w.frameGeometry = { x: x, y: y, width: g.width, height: g.height }; }
    });
  }
  var l = workspace.windowList();
  for (var i = 0; i < l.length; i++) attach(l[i], false);
  workspace.windowAdded.connect(function(w) { attach(w, true); });
})();
"#;
