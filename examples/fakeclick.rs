// Test helper (not part of Lumaris): moves the pointer and clicks through KWin's fake input
// protocol. Used in a nested, hidden KWin to test menus with real pointer events.
//   fakeclick <x> <y> [left|right|move]
//   fakeclick <x> <y> drag <x2> <y2>    (left button held from x,y to x2,y2)
//   fakeclick key <evdev code>          (press and release a key, e.g. 19 = R)
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;

struct S;
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for S {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<OrgKdeKwinFakeInput, ()> for S {
    fn event(
        _: &mut Self,
        _: &OrgKdeKwinFakeInput,
        _: <OrgKdeKwinFakeInput as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let conn = Connection::connect_to_env().expect("wayland");
    let (globals, mut queue) = registry_queue_init::<S>(&conn).unwrap();
    let qh = queue.handle();
    let fake: OrgKdeKwinFakeInput = globals.bind(&qh, 1..=5, ()).expect("fake input");
    fake.authenticate("lumaris-test".into(), "menu test".into());
    let pause = || std::thread::sleep(std::time::Duration::from_millis(50));
    if a[1] == "key" {
        let code: u32 = a[2].parse().unwrap();
        fake.keyboard_key(code, 1);
        queue.roundtrip(&mut S).unwrap();
        pause();
        fake.keyboard_key(code, 0);
        queue.roundtrip(&mut S).unwrap();
        return;
    }
    let (x, y): (f64, f64) = (a[1].parse().unwrap(), a[2].parse().unwrap());
    let what = a.get(3).map(String::as_str).unwrap_or("left");
    fake.pointer_motion_absolute(x, y);
    queue.roundtrip(&mut S).unwrap();
    if what == "drag" {
        let (x2, y2): (f64, f64) = (a[4].parse().unwrap(), a[5].parse().unwrap());
        pause();
        fake.button(0x110, 1);
        queue.roundtrip(&mut S).unwrap();
        for i in 1..=20 {
            pause();
            let t = i as f64 / 20.0;
            fake.pointer_motion_absolute(x + (x2 - x) * t, y + (y2 - y) * t);
            queue.roundtrip(&mut S).unwrap();
        }
        pause();
        fake.button(0x110, 0);
        queue.roundtrip(&mut S).unwrap();
        return;
    }
    if what != "move" {
        let button = if what == "right" { 0x111 } else { 0x110 };
        std::thread::sleep(std::time::Duration::from_millis(50));
        fake.button(button, 1);
        queue.roundtrip(&mut S).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        fake.button(button, 0);
        queue.roundtrip(&mut S).unwrap();
    }
}
