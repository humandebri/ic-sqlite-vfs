use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    let bindings = manifest_dir.join("vendor/sqlite/bindings/sqlite_bindings.rs");
    std::fs::copy(bindings, out_dir.join("sqlite_bindings.rs"))
        .expect("copy vendored SQLite bindings");

    println!("cargo:rerun-if-changed=vendor/sqlite/bindings/sqlite_bindings.rs");
    println!("cargo:rerun-if-changed=vendor/sqlite/build-flags.txt");
    println!("cargo:rerun-if-changed=vendor/sqlite/wasm-compiler-flags.txt");

    let bundled = env::var_os("CARGO_FEATURE_SQLITE_BUNDLED").is_some();
    let precompiled = env::var_os("CARGO_FEATURE_SQLITE_PRECOMPILED").is_some();
    match (bundled, precompiled) {
        (true, false) => build_bundled(&manifest_dir, &target),
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

fn build_bundled(manifest_dir: &Path, target: &str) {
    println!("cargo:rerun-if-changed=vendor/sqlite/src/sqlite3.c");
    println!("cargo:rerun-if-changed=vendor/sqlite/src/sqlite3.h");
    println!("cargo:rerun-if-changed=scripts/wasm32-unknown-unknown-cc.sh");
    let flags = sqlite_flags(manifest_dir);
    let mut build = cc::Build::new();
    build
        .file(manifest_dir.join("vendor/sqlite/src/sqlite3.c"))
        .include(manifest_dir.join("vendor/sqlite/src"))
        .warnings(false);
    for flag in flags {
        build.flag(format!("-D{}", flag));
    }
    if target == "wasm32-unknown-unknown" {
        for flag in sqlite_wasm_compiler_flags(manifest_dir) {
            build.flag(flag);
        }
    }
    build.compile("sqlite3");
}

fn sqlite_flags(manifest_dir: &Path) -> Vec<String> {
    read_flags(manifest_dir, "vendor/sqlite/build-flags.txt")
}

fn sqlite_wasm_compiler_flags(manifest_dir: &Path) -> Vec<String> {
    read_flags(manifest_dir, "vendor/sqlite/wasm-compiler-flags.txt")
}

fn read_flags(manifest_dir: &Path, relative_path: &str) -> Vec<String> {
    let path = manifest_dir.join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {}", path.display(), error))
        .lines()
        .map(str::trim)
        .filter(|flag| !flag.is_empty() && !flag.starts_with('#'))
        .map(str::to_owned)
        .collect()
}
