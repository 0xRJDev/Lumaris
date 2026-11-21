// Blurred desktop behind the window.
//
// Uses the standard ext-background-effect protocol (KWin 6.5+, other compositors adopting it)
// and falls back to KDE's older org_kde_kwin_blur protocol. Without either the window is only
// see-through.
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_compositor::WlCompositor, wl_region::WlRegion, wl_registry, wl_surface::WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1;
use wayland_protocols::ext::background_effect::v1::client::ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1;
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur::OrgKdeKwinBlur;
use wayland_protocols_plasma::blur::client::org_kde_kwin_blur_manager::OrgKdeKwinBlurManager;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use winit::window::Window;

struct State;

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(WlCompositor, WlRegion, ExtBackgroundEffectManagerV1, ExtBackgroundEffectSurfaceV1, OrgKdeKwinBlurManager, OrgKdeKwinBlur);

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

enum Effect {
    Standard(ExtBackgroundEffectSurfaceV1),
    Kde(OrgKdeKwinBlur),
}

pub struct Blur {
    conn: Connection,
    effect: Effect,
    _queue: EventQueue<State>,
}

impl Blur {
    /// Blurs the whole window background. Takes effect with the next presented frame.
    pub fn enable(window: &Window) -> Option<Blur> {
        let RawDisplayHandle::Wayland(display) = window.display_handle().ok()?.as_raw() else { return None };
        let RawWindowHandle::Wayland(handle) = window.window_handle().ok()?.as_raw() else { return None };
        // SAFETY: the display and surface belong to the window, which outlives this object
        // (App drops the blur before the window).
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), handle.surface.as_ptr().cast()) }.ok()?;
        let surface = WlSurface::from_id(&conn, id).ok()?;

        let effect = if let Ok(manager) = globals.bind::<ExtBackgroundEffectManagerV1, _, _>(&qh, 1..=1, ()) {
            let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).ok()?;
            let region = compositor.create_region(&qh, ());
            region.add(0, 0, i32::MAX, i32::MAX); // the whole window, at any size
            let effect = manager.get_background_effect(&surface, &qh, ());
            effect.set_blur_region(Some(&region));
            region.destroy();
            Effect::Standard(effect)
        } else {
            let manager: OrgKdeKwinBlurManager = globals.bind(&qh, 1..=1, ()).ok()?;
            let blur = manager.create(&surface, &qh, ());
            blur.commit(); // no region: the whole surface
            Effect::Kde(blur)
        };
        let _ = queue.roundtrip(&mut State);
        Some(Blur { conn, effect, _queue: queue })
    }
}

impl Drop for Blur {
    fn drop(&mut self) {
        match &self.effect {
            Effect::Standard(e) => e.destroy(),
            Effect::Kde(b) => b.release(),
        }
        let _ = self.conn.flush();
    }
}
