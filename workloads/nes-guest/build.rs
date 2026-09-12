// SPDX-License-Identifier: AGPL-3.0-or-later
//! Link the pinned QuickNES archive only for the static Nova guest profile.

use std::{env, path::Path, process};

fn main() {
    println!("cargo:rerun-if-env-changed=HARMONY_QUICKNES_STATIC_LIB");
    if env::var_os("CARGO_FEATURE_STATIC_QUICKNES").is_none() {
        return;
    }

    let Some(archive) = env::var_os("HARMONY_QUICKNES_STATIC_LIB") else {
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
    println!("cargo:rustc-link-lib=static=quicknes_libretro");
    println!("cargo:rustc-link-lib=m");
}
