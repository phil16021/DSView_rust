//! Compiles the original C sources (exactly the lists in CMakeLists.txt) plus
//! the oracle glue into one static library. Set ORACLE_SANITIZE=1 for
//! ASan/UBSan builds (CI `oracle-diff`).

use std::path::PathBuf;

const LIBSIGROK: &[&str] = &[
    "version.c",
    "strutil.c",
    "std.c",
    "session_driver.c",
    "session.c",
    "log.c",
    "hwdriver.c",
    "error.c",
    "backend.c",
    "output/output.c",
    "input/input.c",
    "hardware/demo/demo.c",
    "input/in_binary.c",
    "input/in_vcd.c",
    "input/in_wav.c",
    "output/csv.c",
    "output/gnuplot.c",
    "output/srzip.c",
    "output/vcd.c",
    "hardware/DSL/dslogic.c",
    "hardware/common/usb.c",
    "hardware/common/ezusb.c",
    "trigger.c",
    "dsdevice.c",
    "hardware/DSL/dscope.c",
    "hardware/DSL/command.c",
    "hardware/DSL/dsl.c",
    "lib_main.c",
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
    for lib in ["glib-2.0", "python3-embed", "libusb-1.0", "zlib"] {
        let found = pkg_config::Config::new()
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
        .warnings(false)
        .opt_level(1)
        .debug(true);
    if sanitize {
        build
            .flag("-fsanitize=address,undefined")
            .flag("-fno-omit-frame-pointer");
        println!("cargo:rustc-link-arg=-fsanitize=address,undefined");
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

    for src in &sources {
        println!("cargo:rerun-if-changed={}", src.display());
    }
    println!("cargo:rerun-if-changed=csrc/oracle.h");
    println!("cargo:rerun-if-env-changed=ORACLE_SANITIZE");

    build.files(&sources).compile("dsview_oracle_c");
    println!("cargo:rustc-link-lib=m");
    println!("cargo:rustc-link-lib=pthread");
}
