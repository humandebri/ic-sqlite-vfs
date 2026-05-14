use std::env;
use std::path::{Path, PathBuf};

const SQLITE_FLAGS: &[&str] = &[
    "SQLITE_CORE",
    "SQLITE_DEFAULT_FOREIGN_KEYS=1",
    "SQLITE_ENABLE_API_ARMOR",
    "SQLITE_USE_URI",
    "SQLITE_OS_OTHER=1",
    "SQLITE_THREADSAFE=0",
    "SQLITE_OMIT_WAL",
    "SQLITE_TEMP_STORE=3",
    "SQLITE_OMIT_DATETIME_FUNCS",
    "SQLITE_OMIT_DEPRECATED",
    "SQLITE_OMIT_LOAD_EXTENSION",
    "SQLITE_OMIT_SHARED_CACHE",
    "SQLITE_DEFAULT_MEMSTATUS=0",
];

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    let bindings = manifest_dir.join("vendor/sqlite/bindings/sqlite_bindings.rs");
    std::fs::copy(bindings, out_dir.join("sqlite_bindings.rs"))
        .expect("copy vendored SQLite bindings");

    println!("cargo:rerun-if-changed=vendor/sqlite/bindings/sqlite_bindings.rs");
    println!("cargo:rerun-if-changed=vendor/sqlite/build-flags.txt");

    let bundled = env::var_os("CARGO_FEATURE_SQLITE_BUNDLED").is_some();
    let precompiled = env::var_os("CARGO_FEATURE_SQLITE_PRECOMPILED").is_some();
    match (bundled, precompiled) {
        (true, false) => build_bundled(&manifest_dir),
        (false, true) => link_precompiled(&manifest_dir, &target),
        (true, true) => {
            panic!("features sqlite-bundled and sqlite-precompiled cannot be enabled together");
        }
        (false, false) => {
            panic!("enable exactly one SQLite feature: sqlite-bundled or sqlite-precompiled");
        }
    }
}

fn link_precompiled(manifest_dir: &Path, target: &str) {
    if target != "wasm32-unknown-unknown" {
        panic!("sqlite-precompiled currently supports only wasm32-unknown-unknown");
    }
    let lib_dir = manifest_dir.join("vendor/sqlite/wasm32-unknown-unknown/lib");
    println!(
        "cargo:rerun-if-changed={}",
        lib_dir.join("libsqlite3.a").display()
    );
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=sqlite3");
}

fn build_bundled(manifest_dir: &Path) {
    println!("cargo:rerun-if-changed=vendor/sqlite/src/sqlite3.c");
    println!("cargo:rerun-if-changed=vendor/sqlite/src/sqlite3.h");
    let mut build = cc::Build::new();
    build
        .file(manifest_dir.join("vendor/sqlite/src/sqlite3.c"))
        .include(manifest_dir.join("vendor/sqlite/src"))
        .warnings(false);
    for flag in SQLITE_FLAGS {
        build.flag(format!("-D{flag}"));
    }
    build.compile("sqlite3");
}
