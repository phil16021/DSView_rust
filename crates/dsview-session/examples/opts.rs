use dsview_core::key;
use dsview_session::{Paths, Session};
use std::sync::Arc;
fn main() {
    let s = Session::new(
        Paths { data_dir: "DSView".into(), profile_dir: None, version: "x".into() },
        Arc::new(dsview_usb::nusb_backend::NusbBackend),
    );
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let (list, _) = s.devices();
    for e in list.iter() {
        if !std::env::args().any(|a| e.name.contains(&a)) { continue; }
        s.set_device(e.handle);
        for m in s.modes() {
            s.switch_work_mode(m as i32);
            println!("== {} mode {:?}", e.name, m);
            let Some(dsview_core::ConfValue::I32s(keys)) = s.config_list(key::DEVICE_OPTIONS) else { println!("no options"); continue };
            for k in keys {
                let info = dsview_session::store::config_info(k);
                println!("  {k} {:?}\n     value {:?}\n     list {:?}", info, s.config(k, None), s.config_list(k));
            }
            let Some(dsview_core::ConfValue::I32s(keys)) = s.config_list(key::PROBE_CONFIGS) else { println!("no probe configs"); continue };
            println!("  probe configs {keys:?}");
        }
    }
    s.exit();
}
