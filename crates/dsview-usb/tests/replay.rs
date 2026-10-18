//! The pcapng/usbmon parser and the replay device on a capture built here:
//! a vendor CTL_RD exchange, a bulk OUT and two bulk IN completions.

use std::time::Duration;

use dsview_usb::replay::{ReplayDevice, compare};
use dsview_usb::trace::{self, Kind};
use dsview_usb::{Setup, UsbDevice, UsbError, VENDOR_IN, VENDOR_OUT};

/// One usbmon (mmapped, 64-byte header) packet.
fn urb(
    id: u64,
    kind: u8,
    xfer: u8,
    ep: u8,
    setup: Option<[u8; 8]>,
    len: u32,
    data: &[u8],
) -> Vec<u8> {
    let mut p = vec![0u8; 64];
    p[0..8].copy_from_slice(&id.to_le_bytes());
    p[8] = kind;
    p[9] = xfer;
    p[10] = ep;
    p[11] = 5; // device
    p[12..14].copy_from_slice(&1u16.to_le_bytes()); // bus
    p[14] = if setup.is_some() { 0 } else { b'-' };
    p[15] = if data.is_empty() { b'<' } else { 0 };
    p[32..36].copy_from_slice(&len.to_le_bytes());
    p[36..40].copy_from_slice(&(data.len() as u32).to_le_bytes());
    if let Some(s) = setup {
        p[40..48].copy_from_slice(&s);
    }
    p.extend_from_slice(data);
    p
}

fn block(ty: u32, body: &[u8]) -> Vec<u8> {
    let mut body = body.to_vec();
    while !body.len().is_multiple_of(4) {
        body.push(0);
    }
    let len = (body.len() + 12) as u32;
    let mut b = ty.to_le_bytes().to_vec();
    b.extend_from_slice(&len.to_le_bytes());
    b.extend_from_slice(&body);
    b.extend_from_slice(&len.to_le_bytes());
    b
}

fn pcapng(packets: &[Vec<u8>]) -> Vec<u8> {
    let mut f = block(
        0x0A0D_0D0A,
        &[
            0x4d, 0x3c, 0x2b, 0x1a, 1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ],
    );
    f.extend(block(1, &[220, 0, 0, 0, 0, 0, 4, 0]));
    for p in packets {
        let mut body = vec![0u8; 20];
        body[12..16].copy_from_slice(&(p.len() as u32).to_le_bytes());
        body[16..20].copy_from_slice(&(p.len() as u32).to_le_bytes());
        body.extend_from_slice(p);
        f.extend(block(6, &body));
    }
    f
}

fn capture(header_offset: u8, bulk: &[u8]) -> Vec<u8> {
    let pre = [VENDOR_OUT, 0xb1, 0, 0, 0, 0, 4, 0];
    let rd = [VENDOR_IN, 0xb2, 0, 0, 0, 0, 1, 0];
    pcapng(&[
        // CTL_RD_PRE HW_STATUS with an unset offset, CTL_RD -> 0x49
        urb(1, b'S', 2, 0x00, Some(pre), 4, &[2, header_offset, 0, 1]),
        urb(1, b'C', 2, 0x00, None, 4, &[]),
        urb(2, b'S', 2, 0x80, Some(rd), 1, &[]),
        urb(2, b'C', 2, 0x80, None, 1, &[0x49]),
        urb(3, b'S', 3, 0x02, None, bulk.len() as u32, bulk),
        urb(3, b'C', 3, 0x02, None, bulk.len() as u32, &[]),
        urb(4, b'S', 3, 0x86, None, 512, &[]),
        // only 4 of 512 bytes captured
        urb(4, b'C', 3, 0x86, None, 512, &[1, 2, 3, 4]),
    ])
}

#[test]
fn parses_usbmon_pcapng() {
    let urbs = trace::parse(&capture(0x7f, &[9; 6])).unwrap();
    assert_eq!(urbs.len(), 8);
    assert_eq!(urbs[0].kind, Kind::Submit);
    assert_eq!(urbs[0].setup.unwrap()[1], 0xb1);
    assert_eq!(urbs[3].data, [0x49]);
    assert_eq!((urbs[7].length, urbs[7].data.len()), (512, 4));
}

#[test]
fn replay_checks_and_answers() {
    let urbs = trace::parse(&capture(0x7f, &[9; 6])).unwrap();
    let mut d = ReplayDevice::new(&urbs, 1, 5);
    let t = Duration::from_millis(10);
    let pre = Setup {
        request_type: VENDOR_OUT,
        request: 0xb1,
        value: 0,
        index: 0,
    };
    let rd = Setup {
        request_type: VENDOR_IN,
        request: 0xb2,
        value: 0,
        index: 0,
    };
    // the unset header offset is not compared
    d.control_out(pre, &[2, 0, 0, 1], t).unwrap();
    assert_eq!(d.control_in(rd, 1, t).unwrap(), [0x49]);
    d.bulk_in_submit(0x86, 512).unwrap();
    // data is due only after the bulk OUT that precedes it in the capture
    assert!(d.bulk_in_wait(0x86, t).is_none());
    assert!(matches!(
        d.bulk_out(0x02, &[9; 5], t),
        Err(UsbError::Mismatch(_))
    ));
    d.bulk_out(0x02, &[9; 6], t).unwrap();
    let c = d.bulk_in_wait(0x86, t).unwrap();
    assert_eq!(&c.data[..4], [1, 2, 3, 4]);
    assert_eq!(c.data.len(), 512, "uncaptured data reads as zeros");
    assert_eq!(d.remaining(), 0);
    assert_eq!(d.log().lock().unwrap().len(), 1);
}

#[test]
fn compare_finds_payload_differences() {
    let a = trace::parse(&capture(0x11, &[9; 6])).unwrap();
    let b = trace::parse(&capture(0x22, &[9; 6])).unwrap();
    assert!(compare(&a, &b).is_empty(), "unset offsets differ only");
    let c = trace::parse(&capture(0x11, &[9, 9, 9, 8, 9, 9])).unwrap();
    let d = compare(&a, &c);
    assert_eq!(d.len(), 1, "{d:?}");
    assert!(d[0].contains("byte 3"), "{d:?}");
}
