//! Crash reports: a panic writes the message, where it happened, a
//! backtrace and the last log lines (up to info, whatever the log level)
//! to `$XDG_STATE_HOME/dsview/crash/` (`~/.local/state` when unset), then
//! the default hook prints it as before.

use std::io::Write;
use std::path::PathBuf;

/// `$XDG_STATE_HOME/dsview/crash`
pub fn dir() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(state.join("dsview/crash"))
}

/// The report of a panic.
pub fn report(app: &str, version: &str, info: &std::panic::PanicHookInfo<'_>) -> String {
    let msg = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".into());
    let thread = std::thread::current();
    let at = info
        .location()
        .map_or_else(|| "unknown".into(), |l| l.to_string());
    let log = dsview_drivers::sys::recent_log();
    format!(
        "{app} {version} ({} {})\nthread '{}' panicked at {at}:\n{msg}\n\nbacktrace:\n{}\n\
         \nlast {} log lines:\n{}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        thread.name().unwrap_or("<unnamed>"),
        std::backtrace::Backtrace::force_capture(),
        log.len(),
        log.join("\n")
    )
}

/// Installs the hook for `app` (the binary's name) at `version`.
pub fn install(app: &'static str, version: &'static str) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(d) = dir()
            && std::fs::create_dir_all(&d).is_ok()
        {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |t| t.as_secs());
            let path = d.join(format!("{app}-{secs}-{}.txt", std::process::id()));
            if let Ok(mut f) = std::fs::File::create(&path)
                && f.write_all(report(app, version, info).as_bytes()).is_ok()
            {
                eprintln!("{app}: crash report written to {}", path.display());
            }
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_panic_leaves_a_report() {
        let text = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let t = text.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            *t.lock().unwrap() = super::report("dsview", "1.2.3", info);
        }));
        dsview_drivers::sys::log(3, "Start collect.");
        dsview_drivers::sys::log(5, "too detailed to keep");
        let r = std::thread::Builder::new()
            .name("worker".into())
            .spawn(|| panic!("boom {}", 42))
            .unwrap()
            .join();
        std::panic::set_hook(previous);
        assert!(r.is_err());
        let text = text.lock().unwrap();
        assert!(text.starts_with("dsview 1.2.3 (linux"), "{text}");
        assert!(
            text.contains("thread 'worker' panicked at crates/dsview-session/src/crash.rs"),
            "{text}"
        );
        assert!(text.contains("\nboom 42\n"), "{text}");
        assert!(text.contains("backtrace:"), "{text}");
        assert!(
            text.contains("log lines:\n") && text.contains("[3] Start collect."),
            "{text}"
        );
        assert!(!text.contains("too detailed"), "{text}");
    }
}
