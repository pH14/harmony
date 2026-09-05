// SPDX-License-Identifier: AGPL-3.0-or-later
//! Link the pinned QuickNES archive only for the static Nova guest profile.

use std::{env, path::Path, process};

fn main() {
    println!("cargo:rerun-if-env-changed=HARMONY_QUICKNES_STATIC_LIB");
    if env::var_os("CARGO_FEATURE_STATIC_QUICKNES").is_none() {
        return;
    }

    let Some(archive) = env::var_os("HARMONY_QUICKNES_STATIC_LIB") else {
        // The quality gate checks the portable library with all features but
        // runs integration and binary tests through the dynamic-core profile.
        // Leave the static symbols unresolved here; the explicit Nova guest
        // build supplies and validates the pinned archive.
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
    // The archive is compiled with exceptions and RTTI disabled and carries
    // its tiny C++ ABI shim (operator new/delete and __cxa_pure_virtual).
    // Do not discover or link the build host's libstdc++ here: a glibc
    // libstdc++.a contains glibc-only entry points such as __isoc23_strtol,
    // which cannot be resolved by a static musl target. The target linker
    // supplies libc and the compiler builtins for the selected architecture.
    println!("cargo:rustc-link-lib=static=quicknes_libretro");
    println!("cargo:rustc-link-lib=m");
}
