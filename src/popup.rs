// Menus that may extend beyond the window (Wayland). winit has no popup windows, and child
// surfaces of the window get no mouse input outside it on KWin, so each menu level is an own
// layer surface (wlr-layer-shell, as used by panels) on an own Wayland connection, placed at
// the window's position on the screen. Their mouse input arrives on an own event queue and is passed to the main thread as
// UserEvent::Popup.
use std::os::fd::AsFd;
use std::sync::{Arc, Mutex};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_pointer::{self, ButtonState, WlPointer};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_seat::WlSeat, wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{Layer as ShellLayer, ZwlrLayerShellV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1};
use winit::event_loop::EventLoopProxy;
use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use winit::window::Window;

use crate::UserEvent;

#[derive(Debug, Clone, Copy)]
pub enum PopupInput {
    /// pointer position in window coordinates (physical pixels)
    Motion(f64, f64),
    /// left (true) or right (false) button pressed at the last position
    Press(bool),
}

/// One menu level on the screen
struct Level {
    surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    /// position in window coordinates (physical pixels) and size (physical pixels)
    x: f64,
    y: f64,
    size: (u32, u32),
    margins: (i32, i32),
    configured: bool,
    /// picture shown, picture waiting for the first configure, previous picture
    buffer: Option<(WlBuffer, WlShmPool)>,
    pending: Option<(WlBuffer, WlShmPool)>,
    old: Option<(WlBuffer, WlShmPool)>,
    checksum: u64,
}

impl Level {
    fn destroy(mut self) {
        for (b, p) in [self.buffer.take(), self.pending.take(), self.old.take()].into_iter().flatten() {
            b.destroy();
            p.destroy();
        }
        self.layer.destroy();
        self.surface.destroy();
    }
}

type Levels = Arc<Mutex<Vec<Level>>>;

struct State {
    proxy: EventLoopProxy<UserEvent>,
    levels: Levels,
    conn: Connection,
    scale: Arc<Mutex<f64>>,
    over: Option<(f64, f64)>, // window position of the level under the pointer
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer, WlSeat, ZwlrLayerShellV1);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(state: &mut Self, layer: &ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if std::env::var_os("LUMARIS_DEBUG_POPUP").is_some() {
            eprintln!("popup layer: {event:?}");
        }
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            layer.ack_configure(serial);
            let mut levels = state.levels.lock().unwrap();
            if let Some(l) = levels.iter_mut().find(|l| l.layer == *layer) {
                l.configured = true;
                if let Some(b) = l.pending.take() {
                    l.surface.attach(Some(&b.0), 0, 0);
                    l.surface.damage(0, 0, i32::MAX, i32::MAX);
                    l.buffer = Some(b);
                }
                l.surface.commit();
            }
            let _ = state.conn.flush();
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(state: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let scale = *state.scale.lock().unwrap();
        if std::env::var_os("LUMARIS_DEBUG_POPUP").is_some() {
            eprintln!("popup pointer: {event:?} over={:?}", state.over);
        }
        let to_window = |o: (f64, f64), sx: f64, sy: f64| (o.0 + sx * scale, o.1 + sy * scale);
        match event {
            wl_pointer::Event::Enter { surface, surface_x, surface_y, .. } => {
                state.over = state.levels.lock().unwrap().iter().find(|l| l.surface == surface).map(|l| (l.x, l.y));
                if let Some(o) = state.over {
                    let (x, y) = to_window(o, surface_x, surface_y);
                    let _ = state.proxy.send_event(UserEvent::Popup(PopupInput::Motion(x, y)));
                }
            }
            wl_pointer::Event::Leave { .. } => state.over = None,
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                if let Some(o) = state.over {
                    let (x, y) = to_window(o, surface_x, surface_y);
                    let _ = state.proxy.send_event(UserEvent::Popup(PopupInput::Motion(x, y)));
                }
            }
            wl_pointer::Event::Button { button, state: WEnum::Value(ButtonState::Pressed), .. } if state.over.is_some() => {
                // BTN_LEFT = 0x110, BTN_RIGHT = 0x111
                if button == 0x110 || button == 0x111 {
                    let _ = state.proxy.send_event(UserEvent::Popup(PopupInput::Press(button == 0x110)));
                }
            }
            _ => {}
        }
    }
}

fn checksum(pixels: &[u8], w: u32, h: u32) -> u64 {
    let mut sum = (w as u64) << 32 | h as u64;
    for (i, c) in pixels.chunks(8).enumerate() {
        let mut v = [0u8; 8];
        v[..c.len()].copy_from_slice(c);
        sum = sum.rotate_left(5) ^ u64::from_le_bytes(v) ^ i as u64;
    }
    sum
}

pub struct Popups {
    conn: Connection,
    qh: QueueHandle<State>,
    compositor: WlCompositor,
    shell: ZwlrLayerShellV1,
    shm: WlShm,
    levels: Levels,
    scale: Arc<Mutex<f64>>,
}

impl Popups {
    /// Available on Wayland compositors with the layer shell (KWin, wlroots based ones).
    pub fn new(window: &Window, proxy: EventLoopProxy<UserEvent>) -> Option<Popups> {
        // only on Wayland
        let RawDisplayHandle::Wayland(_) = window.display_handle().ok()?.as_raw() else { return None };
        // an own connection: winit assumes that every surface on its connection is one of its
        // windows (and panics when the pointer enters another one)
        let conn = Connection::connect_to_env().ok()?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        let compositor: WlCompositor = globals.bind(&qh, 1..=4, ()).ok()?;
        let shell: ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ()).ok()?;
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).ok()?;
        let seat: WlSeat = globals.bind(&qh, 1..=7, ()).ok()?;
        let pointer = seat.get_pointer(&qh, ());
        let levels: Levels = Arc::new(Mutex::new(Vec::new()));
        let scale = Arc::new(Mutex::new(window.scale_factor()));
        let mut state = State { proxy, levels: levels.clone(), conn: conn.clone(), scale: scale.clone(), over: None };
        std::thread::spawn(move || {
            let _keep = (seat, pointer);
            while queue.blocking_dispatch(&mut state).is_ok() {}
        });
        Some(Popups { conn, qh, compositor, shell, shm, levels, scale })
    }

    /// Shows these menu levels: premultiplied RGBA (pixels, width, height, x, y in window
    /// coordinates, physical pixels). `client`: the window content's position in the usable
    /// screen area (logical pixels). Levels are kept; only changed pictures are replaced.
    pub fn show(&mut self, layers: &[(&[u8], u32, u32, i32, i32)], scale: f64, client: (i32, i32)) {
        *self.scale.lock().unwrap() = scale;
        let buffer_scale = scale.round().max(1.0) as i32;
        let mut levels = self.levels.lock().unwrap();
        // a level whose size changed is replaced
        let keep = levels.iter().zip(layers).take_while(|(l, n)| l.size == (n.1, n.2)).count();
        for l in levels.drain(keep..) {
            l.destroy();
        }
        for (i, &(pixels, w, h, x, y)) in layers.iter().enumerate() {
            let margins = (client.0 + (x as f64 / scale).round() as i32, client.1 + (y as f64 / scale).round() as i32);
            if i == levels.len() {
                let surface = self.compositor.create_surface(&self.qh, ());
                if surface.version() >= 3 {
                    surface.set_buffer_scale(buffer_scale);
                }
                // "tooltip": the name decides the window type; KWin repositions ordinary ones
                let layer = self.shell.get_layer_surface(&surface, None, ShellLayer::Top, "tooltip".into(), &self.qh, ());
                layer.set_size((w as f64 / scale).round() as u32, (h as f64 / scale).round() as u32);
                layer.set_anchor(Anchor::Top | Anchor::Left);
                layer.set_margin(margins.1, 0, 0, margins.0);
                layer.set_keyboard_interactivity(KeyboardInteractivity::None);
                surface.commit(); // the compositor answers with a configure
                levels.push(Level {
                    surface,
                    layer,
                    x: x as f64,
                    y: y as f64,
                    size: (w, h),
                    margins,
                    configured: false,
                    buffer: None,
                    pending: None,
                    old: None,
                    checksum: 0,
                });
            }
            let sum = checksum(pixels, w, h);
            let new_buffer = if levels[i].checksum != sum { self.buffer(pixels, w, h, i) } else { None };
            let l = &mut levels[i];
            l.x = x as f64;
            l.y = y as f64;
            let mut changed = false;
            if l.margins != margins {
                l.layer.set_margin(margins.1, 0, 0, margins.0);
                l.margins = margins;
                changed = true;
            }
            if let Some(nb) = new_buffer {
                l.checksum = sum;
                if l.configured {
                    if let Some((b, p)) = l.old.take() {
                        b.destroy();
                        p.destroy();
                    }
                    l.surface.attach(Some(&nb.0), 0, 0);
                    l.surface.damage(0, 0, i32::MAX, i32::MAX);
                    l.old = l.buffer.replace(nb);
                    changed = true;
                } else if let Some((b, p)) = l.pending.replace(nb) {
                    b.destroy();
                    p.destroy();
                }
            }
            if changed && l.configured {
                l.surface.commit();
            }
        }
        drop(levels);
        let _ = self.conn.flush();
    }

    pub fn count(&self) -> usize {
        self.levels.lock().unwrap().len()
    }

    pub fn hide(&mut self) {
        let mut levels = self.levels.lock().unwrap();
        if levels.is_empty() {
            return;
        }
        for l in levels.drain(..) {
            l.destroy();
        }
        drop(levels);
        let _ = self.conn.flush();
    }

    /// A shared memory buffer with the picture
    fn buffer(&self, pixels: &[u8], w: u32, h: u32, n: usize) -> Option<(WlBuffer, WlShmPool)> {
        let stride = w as usize * 4;
        let size = stride * h as usize;
        // shared memory file (created and unlinked right away)
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let path = dir.join(format!("lumaris-popup-{}-{n}", std::process::id()));
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).ok()?;
        let _ = std::fs::remove_file(&path);
        file.set_len(size as u64).ok()?;
        // ARGB8888 is B, G, R, A in memory (little endian); the pixels are premultiplied RGBA
        let mut bgra = Vec::with_capacity(size);
        for p in pixels.chunks_exact(4) {
            bgra.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
        use std::os::unix::fs::FileExt;
        file.write_all_at(&bgra, 0).ok()?;
        let pool = self.shm.create_pool(file.as_fd(), size as i32, &self.qh, ());
        let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, wl_shm::Format::Argb8888, &self.qh, ());
        Some((buffer, pool))
    }
}
