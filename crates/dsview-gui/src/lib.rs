//! DSView's GUI in Rust (docs/migration/PLAN.md, Phase 5): the window
//! (`app`), the time view (`timeview`) and the control API hooks (`hooks`).
//! The binary is `dsview` (`main.rs`).

pub mod analogview;
pub mod analysisdlg;
pub mod app;
pub mod dsotrigger;
pub mod dsoview;
pub mod fftview;
pub mod hooks;
pub mod lissajousview;
pub mod mathview;
pub mod measuredock;
pub mod protocol;
pub mod refview;
pub mod soak;
pub mod timeview;
pub mod toolbar;
pub mod triggerdock;

/// DSView's version (CMakeLists.txt `DS_VERSION_STRING`).
pub const DSVIEW_VERSION: &str = "1.6.1";

/// Prints the `--bench` result.
pub fn report(b: &app::Bench) {
    let mut f = b.frames.clone();
    let mut c = b.cpu.clone();
    f.sort_by(f64::total_cmp);
    c.sort_by(f64::total_cmp);
    let pct = |v: &[f64], p: f64| {
        v.get(((v.len() as f64 - 1.0) * p) as usize)
            .copied()
            .unwrap_or(0.0)
    };
    let renderer = std::env::var("DSVIEW_RENDERER").unwrap_or_else(|_| "glow".into());
    println!(
        "bench {renderer}: {} frames; frame ms median {:.1} p95 {:.1} ({:.1} fps); \
         ui cpu ms median {:.2} p95 {:.2}",
        f.len(),
        pct(&f, 0.5),
        pct(&f, 0.95),
        1000.0 / pct(&f, 0.5).max(1e-9),
        pct(&c, 0.5),
        pct(&c, 0.95),
    );
}
