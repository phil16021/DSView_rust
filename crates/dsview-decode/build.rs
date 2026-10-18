//! Test binaries embed Python: give them an rpath to the libpython of the
//! interpreter PyO3 builds against (pyenv-style installs are not on the
//! loader path).

fn main() {
    println!("cargo:rerun-if-env-changed=PYO3_PYTHON");
    let python = std::env::var("PYO3_PYTHON").unwrap_or_else(|_| "python3".into());
    let Ok(out) = std::process::Command::new(&python)
        .args([
            "-c",
            "import sysconfig; print(sysconfig.get_config_var('LIBDIR'))",
        ])
        .output()
    else {
        return;
    };
    if let Ok(dir) = String::from_utf8(out.stdout) {
        let dir = dir.trim();
        if !dir.is_empty() {
            println!("cargo:rustc-link-arg-tests=-Wl,-rpath,{dir}");
        }
    }
}
