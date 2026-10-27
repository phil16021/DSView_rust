//! The packaged binaries find the bundled libpython in `<prefix>/lib/dsview`
//! (`dsview_decode::python`).

fn main() {
    println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN/../lib/dsview");
}
