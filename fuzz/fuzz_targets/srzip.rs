#![no_main]
//! srzip writing from any channel set and packets: the archive always
//! reopens and holds every packet as its chunk.

use std::io::Read;

use dsview_formats::export::{ChannelKind, ExportChannel, ExportPacket};
use dsview_formats::srzip::SrzipWriter;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&n, rest)) = data.split_first() else {
        return;
    };
    let n = usize::from(n % 33);
    let (names, rest) = rest.split_at(rest.len().min(n));
    let channels: Vec<ExportChannel> = names
        .iter()
        .enumerate()
        .map(|(i, &b)| ExportChannel {
            index: i as u16,
            kind: if b & 0x80 != 0 { ChannelKind::Dso } else { ChannelKind::Logic },
            enabled: b & 1 != 0,
            name: String::from_utf8_lossy(&[b' ', b'\\', b'\t', b'\n', b'a', b]).into_owned(),
            vdiv: 0,
            vfactor: 0,
            hw_offset: 0,
            map_max: 0.0,
            map_min: 0.0,
            map_unit: String::new(),
        })
        .collect();
    let dir = std::env::temp_dir().join(format!("dsv-fuzz-srzip-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("f.srzip");
    let mut w = SrzipWriter::new(&path, channels, [2026, 1, 1, 0, 0, 0]);
    w.receive(&ExportPacket::Meta {
        samplerate: Some(u64::from(n as u32) * 1_000_003),
        limit_samples: None,
        ref_min: None,
        ref_max: None,
    })
    .unwrap();
    let packets: Vec<&[u8]> = rest.chunks(97).collect();
    for p in &packets {
        w.receive(&ExportPacket::Logic {
            data: p,
            unitsize: 1 + p.len() % 4,
            export_original: false,
        })
        .unwrap();
    }
    let written = w.finish().unwrap();
    assert_eq!(written, !packets.is_empty());
    if written {
        let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        assert_eq!(z.len(), packets.len() + 2);
        for (i, p) in packets.iter().enumerate() {
            let mut v = Vec::new();
            z.by_name(&format!("logic-1-{}", i + 1))
                .unwrap()
                .read_to_end(&mut v)
                .unwrap();
            assert_eq!(&v, p);
        }
        let _ = std::fs::remove_file(&path);
    }
});
