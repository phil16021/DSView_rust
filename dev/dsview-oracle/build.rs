//! Compiles the original C sources (exactly the lists in CMakeLists.txt) plus
//! the oracle glue into one static library. Set ORACLE_SANITIZE=1 for
//! ASan/UBSan builds (CI `oracle-diff`).

use std::path::{Path, PathBuf};

const LIBSIGROK: &[&str] = &[
    "version.c",
    "strutil.c",
    "std.c",
    "log.c",
    "hwdriver.c",
    "error.c",
    "backend.c",
    "output/output.c",
    "input/input.c",
    "input/in_binary.c",
    "input/in_vcd.c",
    "input/in_wav.c",
    "output/csv.c",
    "output/gnuplot.c",
    "output/srzip.c",
    "output/vcd.c",
    "hardware/common/usb.c",
    "hardware/common/ezusb.c",
    "trigger.c",
    "dsdevice.c",
    "lib_main.c",
    "hardware/rust/rs_driver.c",
    "output/rs_output.c",
];
const LIBSIGROKDECODE: &[&str] = &[
    "type_decoder.c",
    "srd.c",
    "module_sigrokdecode.c",
    "decoder.c",
    "error.c",
    "exception.c",
    "instance.c",
    "log.c",
    "session.c",
    "util.c",
    "version.c",
    "rs_decode.c",
];
const COMMON: &[&str] = &[
    "minizip/zip.c",
    "minizip/unzip.c",
    "minizip/ioapi.c",
    "log/xlog.c",
];

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("../..").canonicalize().unwrap();
    let sanitize = std::env::var("ORACLE_SANITIZE").is_ok_and(|v| v == "1");

    let mut includes = Vec::new();
    includes.extend(python_embed());
    for lib in ["glib-2.0", "libusb-1.0", "zlib"] {
        // libusb: headers only, csrc/fake_libusb.c stands in for the library
        let found = pkg_config::Config::new()
            .cargo_metadata(lib != "libusb-1.0")
            .probe(lib)
            .unwrap_or_else(|e| panic!("pkg-config {lib}: {e}"));
        includes.extend(found.include_paths);
    }

    let mut build = cc::Build::new();
    build
        .std("c99")
        .define("_DEFAULT_SOURCE", None)
        .includes(&includes)
        .include(root.join("DSView"))
        .include(root.join("libsigrok4DSL"))
        .include(root.join("libsigrokdecode4DSL"))
        .include(root.join("common"))
        .include(manifest.join("csrc"))
        .include(root.join("crates/dsview-ffi/include"))
        // the Rust implementations behind DSVIEW_IMPL_* (dsview-ffi, linked in)
        .define("DSVIEW_RUST_CORE", "1")
        // DSView's CMakeLists forces a Release build: asserts are off in the app
        .define("NDEBUG", None)
        .warnings(false)
        .opt_level(1)
        .debug(true);
    if sanitize {
        build
            .flag("-fsanitize=address,undefined")
            .flag("-fno-omit-frame-pointer");
        println!("cargo:rustc-link-arg=-fsanitize=address,undefined");
        // rustc links with -nodefaultlibs, so name the runtimes explicitly
        println!("cargo:rustc-link-lib=dylib=asan");
        println!("cargo:rustc-link-lib=dylib=ubsan");
    }

    let mut sources: Vec<PathBuf> = Vec::new();
    sources.extend(LIBSIGROK.iter().map(|f| root.join("libsigrok4DSL").join(f)));
    sources.extend(
        LIBSIGROKDECODE
            .iter()
            .map(|f| root.join("libsigrokdecode4DSL").join(f)),
    );
    sources.extend(COMMON.iter().map(|f| root.join("common").join(f)));
    sources.push(manifest.join("csrc/oracle_decode.c"));
    sources.push(manifest.join("csrc/oracle_dsl.c"));
    sources.push(manifest.join("csrc/oracle_export.c"));
    sources.push(manifest.join("csrc/oracle_drv.c"));
    sources.push(manifest.join("csrc/fake_libusb.c"));

    for src in &sources {
        println!("cargo:rerun-if-changed={}", src.display());
    }
    println!("cargo:rerun-if-changed=csrc/oracle.h");
    println!("cargo:rerun-if-env-changed=ORACLE_SANITIZE");

    // The demo driver runs on the oracle's virtual clock and random sequence
    // (shared with the Rust driver). It and the session driver leave packet
    // fields uninitialized (DEVIATIONS.md F20); zeroed here, as Rust sends them.
    let demo = root.join("libsigrok4DSL/hardware/demo/demo.c");
    let session_driver = root.join("libsigrok4DSL/session_driver.c");
    println!("cargo:rerun-if-changed={}", demo.display());
    println!("cargo:rerun-if-changed={}", session_driver.display());
    let mut demo_build = build.clone();
    for (from, to) in [
        ("g_usleep", "oracle_usleep"),
        ("g_timer_new", "oracle_timer_new"),
        ("g_timer_start", "oracle_timer_start"),
        ("g_timer_elapsed", "oracle_timer_elapsed"),
        ("rand", "oracle_rand"),
        ("srand", "oracle_srand"),
    ] {
        demo_build.define(from, to);
    }
    let demo_objs = demo_build
        .flag("-ftrivial-auto-var-init=zero")
        .file(&demo)
        .file(&session_driver)
        .compile_intermediates();

    // The DSL drivers run against the simulated device of src/usbsim.rs on
    // the virtual clock; their uninitialized command headers (F22) are
    // zeroed as Rust sends them. session.c waits in the fake libusb's poll.
    let dsl: Vec<PathBuf> = [
        "hardware/DSL/dsl.c",
        "hardware/DSL/dscope.c",
        "hardware/DSL/dslogic.c",
        "hardware/DSL/command.c",
    ]
    .iter()
    .map(|f| root.join("libsigrok4DSL").join(f))
    .collect();
    for f in &dsl {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    let dsl_objs = build
        .clone()
        .define("g_usleep", "oracle_usleep")
        .flag("-ftrivial-auto-var-init=zero")
        .files(&dsl)
        .compile_intermediates();
    let session = root.join("libsigrok4DSL/session.c");
    println!("cargo:rerun-if-changed={}", session.display());
    let session_objs = build
        .clone()
        .define("g_poll", "oracle_poll")
        .file(&session)
        .compile_intermediates();

    build
        .files(&sources)
        .objects(demo_objs)
        .objects(dsl_objs)
        .objects(session_objs)
        .compile("dsview_oracle_c");
    build_store(&root, &manifest, &includes, sanitize);
    println!("cargo:rustc-link-lib=m");
    println!("cargo:rustc-link-lib=pthread");
}

/// pv::data::{Logic,Dso,Analog}Snapshot, compiled unchanged against Qt Core. DSView's
/// CMakeLists forces a Release build, so NDEBUG is set as in the shipped app.
fn build_store(root: &Path, manifest: &Path, includes: &[PathBuf], sanitize: bool) {
    let qt = pkg_config::Config::new()
        .probe("Qt6Core")
        .or_else(|_| pkg_config::Config::new().probe("Qt5Core"))
        .expect("pkg-config Qt6Core or Qt5Core");

    // dsvdef.h includes "DSView/config.h", which CMake generates.
    let gen_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("gen");
    std::fs::create_dir_all(gen_dir.join("DSView")).unwrap();
    std::fs::write(
        gen_dir.join("DSView/config.h"),
        "#pragma once\n#define DS_TITLE \"DSView\"\n#define DS_VERSION_STRING \"oracle\"\n",
    )
    .unwrap();

    let sources = [
        root.join("DSView/pv/data/snapshot.cpp"),
        root.join("DSView/pv/data/logicsnapshot.cpp"),
        root.join("DSView/pv/data/dsosnapshot.cpp"),
        root.join("DSView/pv/data/analogsnapshot.cpp"),
        manifest.join("csrc/oracle_store.cpp"),
        manifest.join("csrc/oracle_store_ad.cpp"),
    ];
    for src in &sources {
        println!("cargo:rerun-if-changed={}", src.display());
    }

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .define("NDEBUG", None)
        .define("QT_NO_KEYWORDS", None)
        .includes(includes)
        .includes(&qt.include_paths)
        .include(&gen_dir)
        .include(root.join("DSView"))
        .include(root.join("DSView/pv"))
        .include(root.join("libsigrok4DSL"))
        .include(root.join("common"))
        .include(manifest.join("csrc"))
        .flag("-fPIC")
        .warnings(false)
        .opt_level(2)
        .debug(true);
    if sanitize {
        build
            .flag("-fsanitize=address,undefined")
            .flag("-fno-omit-frame-pointer");
    }
    build.files(&sources).compile("dsview_oracle_store");
}

/// The Python the C engine embeds: the interpreter PyO3 uses as well
/// (`PYO3_PYTHON`, else `python3` on PATH), so one process can host both
/// engines with a single libpython. Returns its include directory.
fn python_embed() -> Vec<PathBuf> {
    println!("cargo:rerun-if-env-changed=PYO3_PYTHON");
    let python = std::env::var("PYO3_PYTHON").unwrap_or_else(|_| "python3".into());
    let out = std::process::Command::new(&python)
        .args([
            "-c",
            "import sysconfig as s; print(s.get_config_var('INCLUDEPY')); \
             print(s.get_config_var('LIBDIR')); print(s.get_config_var('LDVERSION'))",
        ])
        .output()
        .unwrap_or_else(|e| panic!("running {python}: {e}"));
    assert!(out.status.success(), "{python} sysconfig query failed");
    let text = String::from_utf8(out.stdout).expect("utf-8");
    let mut lines = text.lines();
    let (include, libdir, ldversion) = (
        lines.next().expect("INCLUDEPY"),
        lines.next().expect("LIBDIR"),
        lines.next().expect("LDVERSION"),
    );
    println!("cargo:rustc-link-search=native={libdir}");
    println!("cargo:rustc-link-lib=python{ldversion}");
    // pyenv and similar installs are not on the loader path
    println!("cargo:rustc-link-arg=-Wl,-rpath,{libdir}");
    vec![PathBuf::from(include)]
}
