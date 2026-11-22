// Menus that may extend beyond the window (Wayland). winit has no popup windows, so the menu
// levels are shown on child surfaces of the window (wl_subsurface), which the compositor draws
// outside the window's area as well. Their mouse input arrives on an own event queue and is
// passed to the main thread as UserEvent::Popup.
use std::os::fd::AsFd;
use std::sync::{Arc, Mutex};

use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_pointer::{self, ButtonState, WlPointer};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_seat::WlSeat, wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool, wl_subcompositor::WlSubcompositor, wl_subsurface::WlSubsurface, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use winit::event_loop::EventLoopProxy;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use winit::window::Window;

use crate::UserEvent;

#[derive(Debug, Clone, Copy)]
pub enum PopupInput {
    /// pointer position in window coordinates (physical pixels)
    Motion(f64, f64),
    /// left (true) or right (false) button pressed at the last position
    Press(bool),
}

/// Our surfaces and their position (window coordinates, physical pixels)
type Shown = Arc<Mutex<Vec<(WlSurface, f64, f64)>>>;

struct State {
    proxy: EventLoopProxy<UserEvent>,
    shown: Shown,
    scale: Arc<Mutex<f64>>,
    over: Option<(f64, f64)>, // offset of the surface under the pointer
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(WlCompositor, WlSubcompositor, WlSubsurface, WlSurface, WlShm, WlShmPool, WlBuffer, WlSeat);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlPointer, ()> for State {
    fn event(state: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let scale = *state.scale.lock().unwrap();
        match event {
            wl_pointer::Event::Enter { surface, surface_x, surface_y, .. } => {
                state.over = state.shown.lock().unwrap().iter().find(|(s, ..)| *s == surface).map(|(_, x, y)| (*x, *y));
                if let Some((ox, oy)) = state.over {
                    let _ = state.proxy.send_event(UserEvent::Popup(PopupInput::Motion(ox + surface_x * scale, oy + surface_y * scale)));
                }
            }
            wl_pointer::Event::Leave { .. } => state.over = None,
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                if let Some((ox, oy)) = state.over {
                    let _ = state.proxy.send_event(UserEvent::Popup(PopupInput::Motion(ox + surface_x * scale, oy + surface_y * scale)));
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

struct Layer {
    surface: WlSurface,
    subsurface: WlSubsurface,
    /// buffer shown now and the one before it (kept until the compositor took the new one)
    buffer: Option<(WlBuffer, WlShmPool)>,
    old: Option<(WlBuffer, WlShmPool)>,
    position: (i32, i32),
    /// fingerprint of the picture shown
    checksum: u64,
}

impl Layer {
    fn destroy(mut self) {
        for (b, p) in [self.buffer.take(), self.old.take()].into_iter().flatten() {
            b.destroy();
            p.destroy();
        }
        self.subsurface.destroy();
        self.surface.destroy();
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
    parent: WlSurface,
    compositor: WlCompositor,
    subcompositor: WlSubcompositor,
    shm: WlShm,
    shown: Shown,
    scale: Arc<Mutex<f64>>,
    layers: Vec<Layer>,
}

impl Popups {
    /// Available on Wayland compositors with subsurfaces (all of them).
    pub fn new(window: &Window, proxy: EventLoopProxy<UserEvent>) -> Option<Popups> {
        let RawDisplayHandle::Wayland(display) = window.display_handle().ok()?.as_raw() else { return None };
        let RawWindowHandle::Wayland(handle) = window.window_handle().ok()?.as_raw() else { return None };
        // SAFETY: display and surface belong to the window, which lives until the program ends
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), handle.surface.as_ptr().cast()) }.ok()?;
        let parent = WlSurface::from_id(&conn, id).ok()?;
        let compositor: WlCompositor = globals.bind(&qh, 1..=4, ()).ok()?;
        let subcompositor: WlSubcompositor = globals.bind(&qh, 1..=1, ()).ok()?;
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).ok()?;
        let seat: WlSeat = globals.bind(&qh, 1..=7, ()).ok()?;
        let pointer = seat.get_pointer(&qh, ());
        let shown: Shown = Arc::new(Mutex::new(Vec::new()));
        let scale = Arc::new(Mutex::new(window.scale_factor()));
        let mut state = State { proxy, shown: shown.clone(), scale: scale.clone(), over: None };
        std::thread::spawn(move || {
            let _keep = (seat, pointer);
            while queue.blocking_dispatch(&mut state).is_ok() {}
        });
        Some(Popups { conn, qh, parent, compositor, subcompositor, shm, shown, scale, layers: Vec::new() })
    }

    /// Shows these menu levels: premultiplied RGBA (pixels, width, height, x, y in window
    /// coordinates, physical pixels). Surfaces are kept and only changed pictures are replaced;
    /// the changes appear together with the window's next frame (synchronized subsurfaces).
    pub fn show(&mut self, layers: &[(&[u8], u32, u32, i32, i32)], scale: f64) {
        *self.scale.lock().unwrap() = scale;
        let buffer_scale = scale.round().max(1.0) as i32;
        while self.layers.len() > layers.len() {
            if let Some(l) = self.layers.pop() {
                l.destroy();
            }
        }
        let mut shown = Vec::new();
        for (i, &(pixels, w, h, x, y)) in layers.iter().enumerate() {
            if i == self.layers.len() {
                let surface = self.compositor.create_surface(&self.qh, ());
                let subsurface = self.subcompositor.get_subsurface(&surface, &self.parent, &self.qh, ());
                subsurface.set_sync();
                subsurface.place_above(&self.parent);
                if surface.version() >= 3 {
                    surface.set_buffer_scale(buffer_scale);
                }
                self.layers.push(Layer { surface, subsurface, buffer: None, old: None, position: (i32::MIN, i32::MIN), checksum: 0 });
            }
            let pos = ((x as f64 / scale).round() as i32, (y as f64 / scale).round() as i32);
            let sum = checksum(pixels, w, h);
            let new_buffer = if self.layers[i].checksum != sum { self.buffer(pixels, w, h, i) } else { None };
            let layer = &mut self.layers[i];
            if layer.position != pos {
                layer.subsurface.set_position(pos.0, pos.1);
                layer.position = pos;
            }
            if let Some(nb) = new_buffer {
                if let Some((b, p)) = layer.old.take() {
                    b.destroy();
                    p.destroy();
                }
                layer.surface.attach(Some(&nb.0), 0, 0);
                layer.surface.damage(0, 0, i32::MAX, i32::MAX);
                layer.old = layer.buffer.replace(nb);
                layer.checksum = sum;
            }
            layer.surface.commit();
            shown.push((layer.surface.clone(), x as f64, y as f64));
        }
        *self.shown.lock().unwrap() = shown;
        let _ = self.conn.flush();
    }

    pub fn count(&self) -> usize {
        self.layers.len()
    }

    pub fn hide(&mut self) {
        if self.layers.is_empty() {
            return;
        }
        for l in self.layers.drain(..) {
            l.destroy();
        }
        self.shown.lock().unwrap().clear();
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
