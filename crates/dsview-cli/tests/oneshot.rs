//! The one-shot commands on the demo devices, through the binary, with
//! their own settings directories.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn cli(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_dsview-cli"))
        .args(args)
        .args([
            "--data-dir",
            root().join("DSView").to_str().unwrap(),
            "--log-level",
            "0",
        ])
        .env("XDG_CONFIG_HOME", dir.join("cfg"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn capture_export_and_decode_the_demo() {
    let dir = std::env::temp_dir().join(format!("dsv-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (ok, out, err) = cli(&dir, &["devices"]);
    assert!(ok, "{err}");
    assert!(out.contains("Demo Logic"), "{out}");

    let dsl = dir.join("a.dsl");
    let (ok, _, err) = cli(
        &dir,
        &[
            "capture",
            "--device",
            "demo logic",
            "--out",
            dsl.to_str().unwrap(),
        ],
    );
    assert!(ok, "{err}");
    assert!(dsl.is_file());

    let sr = dir.join("a.srzip");
    let (ok, _, err) = cli(
        &dir,
        &["export", dsl.to_str().unwrap(), sr.to_str().unwrap()],
    );
    assert!(ok, "{err}");
    assert!(sr.is_file());

    let (ok, out, err) = cli(
        &dir,
        &[
            "decode",
            dsl.to_str().unwrap(),
            "--decoder",
            "uart:rxtx=5,baudrate=115200",
        ],
    );
    assert!(ok, "{err}");
    assert!(out.lines().count() > 10, "{out}");
    assert!(out.lines().all(|l| l.contains("UART")), "{out}");

    // errors are the control API's, with exit code 1
    let (ok, _, err) = cli(
        &dir,
        &[
            "decode",
            dsl.to_str().unwrap(),
            "--decoder",
            "uart:rxtx=5,nosuch=1",
        ],
    );
    assert!(!ok);
    assert!(err.contains("has no option 'nosuch'"), "{err}");
    let (ok, _, err) = cli(&dir, &["verify-counter", "--device", "demo logic"]);
    assert!(!ok);
    assert!(err.contains("needs a DSLogic"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
