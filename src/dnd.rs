// Drag and drop of files onto the window (Wayland data device; winit has no drag and drop on
// Wayland). Runs its own event queue on a background thread; dropped files arrive as
// UserEvent::Open.
use std::io::Read;
use std::sync::{Arc, Mutex};

use wayland_client::backend::Backend;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
use winit::event_loop::EventLoopProxy;
use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use winit::window::Window;

use crate::UserEvent;

const URI_LIST: &str = "text/uri-list";

struct State {
    proxy: EventLoopProxy<UserEvent>,
    conn: Connection,
    /// offers and the types they provide
    offers: Arc<Mutex<Vec<(WlDataOffer, Vec<String>)>>>,
    current: Option<WlDataOffer>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WlSeat, ()> for State {
    fn event(_: &mut Self, _: &WlSeat, _: <WlSeat as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(_: &mut Self, _: &WlDataDeviceManager, _: <WlDataDeviceManager as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(state: &mut Self, offer: &WlDataOffer, event: wl_data_offer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            let mut offers = state.offers.lock().unwrap();
            if let Some(o) = offers.iter_mut().find(|(o, _)| o == offer) {
                o.1.push(mime_type);
            } else {
                offers.push((offer.clone(), vec![mime_type]));
            }
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(state: &mut Self, _: &WlDataDevice, event: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_data_device::Event::Enter { serial, id, .. } => {
                let files = id.as_ref().is_some_and(|o| {
                    state.offers.lock().unwrap().iter().any(|(x, types)| x == o && types.iter().any(|t| t == URI_LIST))
                });
                if let (Some(offer), true) = (id, files) {
                    offer.accept(serial, Some(URI_LIST.into()));
                    if offer.version() >= 3 {
                        offer.set_actions(DndAction::Copy, DndAction::Copy);
                    }
                    state.current = Some(offer);
                } else if let Some(offer) = state.current.take() {
                    offer.accept(serial, None);
                }
            }
            wl_data_device::Event::Leave => state.current = None,
            wl_data_device::Event::Drop => {
                let Some(offer) = state.current.take() else { return };
                let Ok((mut reader, writer)) = std::io::pipe() else { return };
                use std::os::fd::AsFd;
                offer.receive(URI_LIST.into(), writer.as_fd());
                drop(writer);
                let _ = state.conn.flush();
                let (proxy, conn, offers) = (state.proxy.clone(), state.conn.clone(), state.offers.clone());
                std::thread::spawn(move || {
                    let mut text = String::new();
                    let _ = reader.read_to_string(&mut text);
                    if offer.version() >= 3 {
                        offer.finish();
                    }
                    offer.destroy();
                    offers.lock().unwrap().retain(|(o, _)| *o != offer);
                    let _ = conn.flush();
                    let paths: Vec<_> = text
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.starts_with('#') && !l.is_empty())
                        .filter_map(crate::portal::uri_to_path)
                        .collect();
                    if let Some(first) = paths.into_iter().next() {
                        let _ = proxy.send_event(UserEvent::Open(first));
                    }
                });
            }
            wl_data_device::Event::Selection { id } => {
                // clipboard offers are not used
                if let Some(o) = id {
                    state.offers.lock().unwrap().retain(|(x, _)| *x != o);
                    o.destroy();
                }
            }
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

/// Starts accepting dropped files (Wayland only). Returns false when not available.
pub fn start(window: &Window, proxy: EventLoopProxy<UserEvent>) -> bool {
    let Ok(handle) = window.display_handle() else { return false };
    let RawDisplayHandle::Wayland(display) = handle.as_raw() else { return false };
    // SAFETY: the display connection lives as long as the window (the whole program)
    let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
    let conn = Connection::from_backend(backend);
    let Ok((globals, mut queue)) = registry_queue_init::<State>(&conn) else { return false };
    let qh = queue.handle();
    let (Ok(seat), Ok(manager)) = (globals.bind::<WlSeat, _, _>(&qh, 1..=7, ()), globals.bind::<WlDataDeviceManager, _, _>(&qh, 1..=3, ()))
    else {
        return false;
    };
    let _device = manager.get_data_device(&seat, &qh, ());
    let mut state = State { proxy, conn: conn.clone(), offers: Arc::new(Mutex::new(Vec::new())), current: None };
    std::thread::spawn(move || {
        let _keep = (seat, manager, _device);
        while queue.blocking_dispatch(&mut state).is_ok() {}
    });
    true
}
