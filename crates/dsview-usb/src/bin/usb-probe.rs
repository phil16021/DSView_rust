//! Lists USB devices; with `--dsl BUS.ADDR` reads a DreamSourceLab device's
//! firmware version (CTL_RD_PRE/CTL_RD FW_VERSION), which changes nothing.

use std::time::Duration;

use dsview_usb::nusb_backend::NusbBackend;
use dsview_usb::{Open, Setup, UsbBackend, VENDOR_IN, VENDOR_OUT};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let b = NusbBackend;
    if let Some(at) = args.iter().position(|a| a == "--dsl") {
        let (bus, addr) = args[at + 1].split_once('.').expect("BUS.ADDR");
        let mut d = b
            .open(bus.parse().unwrap(), addr.parse().unwrap(), Open::Claim(0))
            .expect("open");
        let t = Duration::from_millis(3000);
        // ctl_header {dest FW_VERSION, offset 0, size 2}
        d.control_out(
            Setup {
                request_type: VENDOR_OUT,
                request: 0xb1,
                value: 0,
                index: 0,
            },
            &[0, 0, 0, 2],
            t,
        )
        .expect("CTL_RD_PRE");
        std::thread::sleep(Duration::from_millis(10));
        let v = d
            .control_in(
                Setup {
                    request_type: VENDOR_IN,
                    request: 0xb2,
                    value: 0,
                    index: 0,
                },
                2,
                t,
            )
            .expect("CTL_RD");
        println!("firmware {}.{}", v[0], v[1]);
        return;
    }
    for i in b.list().expect("list") {
        println!(
            "{:03}.{:03} {:04x}:{:04x} {:?} {:?} {:?}",
            i.bus, i.address, i.vid, i.pid, i.speed, i.manufacturer, i.product
        );
    }
}
