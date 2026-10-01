// SPDX-License-Identifier: AGPL-3.0-or-later
use std::{env, path::PathBuf, process::Command};
fn main() {
    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let rustc = env::var_os("RUSTC").expect("Cargo supplies RUSTC");
    let compiler = Command::new(rustc)
        .arg("-V")
        .output()
        .expect("read compiler identity");
    assert!(compiler.status.success());
    let compiler = String::from_utf8(compiler.stdout).unwrap();
    if env::var_os("CARGO_CFG_MIRI").is_none() && env::var_os("MIRI_SYSROOT").is_none() {
        assert_eq!(
            compiler.trim(),
            "rustc 1.97.0 (2d8144b78 2026-07-07)",
            "runtime requires the qualified Rust compiler"
        );
    }
    println!("cargo:rustc-env=HARMONY_WASMI_COMPILER={}", compiler.trim());
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    for path in [
        "prepare.py",
        "numerical.patch",
        "import-completion.patch",
        "validation.patch",
        "debug-positions.patch",
        "wasmi-0.46.0.crate",
        "../qualification/prepare-wasmi.py",
        "../qualification/wasmi-snapshot.patch",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    let result = Command::new("python3")
        .arg(source.join("prepare.py"))
        .arg(source)
        .arg(output)
        .status()
        .expect("Python 3 is required to prepare the pinned runtime source");
    assert!(result.success(), "runtime source preparation failed");
}
