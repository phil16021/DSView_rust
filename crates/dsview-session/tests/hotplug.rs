//! Hotplug in the acquisition library, against a simulated DSLogic Plus
//! (dsview_usb::sim): a short loss of contact (reconnect merge), a real
//! detach of the active device, a new device, and a detach during a capture.
//! The hotplug thread runs in real time (100 ms ticks), as in C.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use dsview_core::{Packet, dev_status};
use dsview_session::{Handle, Host, Kind, Library, event};
use dsview_usb::Speed;
use dsview_usb::sim::{Hub, SharedHub, SimBackend, SimConfig};

#[derive(Default)]
struct TestHost {
    events: Mutex<Vec<i32>>,
    packets: Mutex<Vec<u16>>,
}

impl Host for TestHost {
    fn event(&self, ev: i32) {
        self.events.lock().unwrap().push(ev);
    }
    fn packets(&self, _h: Handle, packets: &[Packet]) {
        self.packets
            .lock()
            .unwrap()
            .extend(packets.iter().map(Packet::type_id));
    }
}

impl TestHost {
    fn has_event(&self, ev: i32) -> bool {
        self.events.lock().unwrap().contains(&ev)
    }
}

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn setup() -> (Arc<TestHost>, Arc<SimBackend>, SharedHub, Library) {
    let hub: SharedHub = Arc::new(Mutex::new(Hub::new(SimConfig::new(
        0x2A0E,
        0x0034,
        Speed::High,
    ))));
    let sim = Arc::new(SimBackend::new(
        hub.clone(),
        Arc::new(|d: Duration| std::thread::sleep(d)),
    ));
    let host = Arc::new(TestHost::default());
    let lib = Library::init(host.clone(), sim.clone());
    (host, sim, hub, lib)
}

fn dslogic(lib: &Library) -> Option<(Handle, Kind)> {
    lib.devices()
        .into_iter()
        .find(|e| matches!(e.kind, Kind::Usb { .. }))
        .map(|e| (e.handle, e.kind))
}

/// Waits up to 3 s for `cond`.
fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !cond() {
        assert!(t0.elapsed() < Duration::from_secs(3), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn reconnect_detach_and_attach() {
    let _g = serial();
    let (host, sim, _hub, lib) = setup();
    // the demo devices, then the DSLogic
    let names: Vec<String> = lib.devices().into_iter().map(|e| e.name).collect();
    assert_eq!(names.len(), 3, "{names:?}");
    let (h, _) = dslogic(&lib).expect("DSLogic listed");
    lib.activate(h).unwrap();
    assert_eq!(lib.active().unwrap().handle, h);
    assert_eq!(
        lib.active().unwrap().lock().info().status,
        dev_status::ACTIVE
    );

    // contact lost for 150 ms: the same device at its new address, open again
    sim.plug(false, None);
    std::thread::sleep(Duration::from_millis(150));
    sim.plug(true, Some(9));
    wait_for("reopened at 1.9", || {
        dslogic(&lib).is_some_and(|(hh, k)| {
            hh == h
                && k == Kind::Usb {
                    family: dsview_drivers::dsl::device::Family::DsLogic,
                    bus: 1,
                    address: 9,
                }
        }) && lib.active().unwrap().lock().info().status == dev_status::ACTIVE
    });
    std::thread::sleep(Duration::from_millis(700));
    assert!(!host.has_event(event::CURRENT_DEVICE_DETACH));
    assert_eq!(dslogic(&lib).unwrap().0, h);

    // gone for good: out of the list after 500 ms, the active one kept
    sim.plug(false, None);
    wait_for("detach event", || {
        host.has_event(event::CURRENT_DEVICE_DETACH)
    });
    assert!(dslogic(&lib).is_none());
    assert_eq!(lib.active().unwrap().handle, h);

    // back: a new device
    sim.plug(true, Some(10));
    wait_for("attach event", || host.has_event(event::NEW_DEVICE_ATTACH));
    let (h2, _) = dslogic(&lib).expect("listed again");
    assert_ne!(h2, h);
    lib.activate(h2).unwrap();
    assert_eq!(lib.active().unwrap().handle, h2);
    lib.exit();
}

#[test]
fn detach_during_capture() {
    let _g = serial();
    let (host, sim, hub, lib) = setup();
    let (h, _) = dslogic(&lib).unwrap();
    lib.activate(h).unwrap();
    // the device waits for its trigger: no data
    hub.lock().unwrap().dev.hold = true;
    lib.start_collect(true).unwrap();
    wait_for("running", || host.has_event(event::DEVICE_RUNNING));
    assert!(lib.is_collecting());

    sim.plug(false, None);
    wait_for("capture ended", || !lib.is_collecting());
    wait_for("ended by detach", || {
        host.has_event(event::COLLECT_TASK_END_BY_DETACHED)
    });
    let p = host.packets.lock().unwrap().clone();
    assert_eq!(p.first(), Some(&10000), "header first: {p:?}");
    assert_eq!(p.last(), Some(&10001), "END last: {p:?}");
    wait_for("detach event", || {
        host.has_event(event::CURRENT_DEVICE_DETACH)
    });
    lib.exit();
}
