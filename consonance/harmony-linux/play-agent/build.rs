// SPDX-License-Identifier: AGPL-3.0-or-later
//! Link the pinned QuickNES archive only for the static Nova guest profile.

use std::{env, path::Path, process, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=HARMONY_QUICKNES_STATIC_LIB");
    if env::var_os("CARGO_FEATURE_STATIC_QUICKNES").is_none() {
        return;
    }

    let Some(archive) = env::var_os("HARMONY_QUICKNES_STATIC_LIB") else {
        // `cargo clippy/test --all-features` exercises this crate without the
        // external, pinned Nova build products. Those targets do not link the
        // production agent executable, so leave the static symbols unresolved
        // and let the explicit guest build supply and validate the archive.
        println!(
            "cargo:warning=static-quicknes executable builds require \
             HARMONY_QUICKNES_STATIC_LIB"
        );
        return;
    };
    let archive = Path::new(&archive);
    let Some(parent) = archive.parent() else {
        eprintln!("QuickNES archive has no parent directory: {archive:?}");
        process::exit(1);
    };
    if archive.file_name().and_then(|name| name.to_str()) != Some("libquicknes_libretro.a") {
        eprintln!("QuickNES archive must be named libquicknes_libretro.a: {archive:?}");
        process::exit(1);
    }

    println!("cargo:rerun-if-changed={}", archive.display());
    println!("cargo:rustc-link-search=native={}", parent.display());
    let compiler_lib = Command::new("c++")
        .arg("-print-file-name=libstdc++.a")
        .output();
    let Ok(compiler_lib) = compiler_lib else {
        eprintln!("static-quicknes could not query c++ for libstdc++.a");
        process::exit(1);
    };
    let compiler_lib = String::from_utf8_lossy(&compiler_lib.stdout);
    let compiler_lib = Path::new(compiler_lib.trim());
    if !compiler_lib.is_absolute() || !compiler_lib.is_file() {
        eprintln!("c++ did not locate a usable libstdc++.a: {compiler_lib:?}");
        process::exit(1);
    }
    let Some(compiler_lib_dir) = compiler_lib.parent() else {
        eprintln!("c++ did not report an absolute libstdc++.a path: {compiler_lib:?}");
        process::exit(1);
    };
    println!(
        "cargo:rustc-link-search=native={}",
        compiler_lib_dir.display()
    );
    println!("cargo:rustc-link-lib=static=quicknes_libretro");
    println!("cargo:rustc-link-lib=static=stdc++");
    println!("cargo:rustc-link-lib=static=gcc_eh");
    println!("cargo:rustc-link-lib=static=gcc");
    println!("cargo:rustc-link-lib=m");
}
