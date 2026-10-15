//! Links the wasm build against the CPython that fetch-cpython.sh downloads.
//! The build for this machine only prints the manifest and runs no Python, so
//! it links nothing.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=cpython");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("wasi") {
        return;
    }
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("cpython");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        panic!("{} is missing: run ./fetch-cpython.sh first", dir.display());
    };
    println!("cargo:rustc-link-search=native={}", dir.display());
    // libpython, the libraries its built-in modules use (expat, HACL*), and
    // the WASI emulation of signals, getpid and process clocks.
    for entry in entries {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if let Some(lib) = name.strip_prefix("lib").and_then(|n| n.strip_suffix(".a")) {
            println!("cargo:rustc-link-lib=static={lib}");
        }
    }
}
