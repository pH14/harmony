// SPDX-License-Identifier: AGPL-3.0-or-later
//! Inject the payload linker script.
//!
//! This lives in the *bin* package rather than in `runtime` because
//! `cargo::rustc-link-arg-bins` only applies to binaries of the package whose
//! build script emitted it.

use std::path::PathBuf;

fn main() {
    // CARGO_MANIFEST_DIR is oracles/; the script sits at the payloads workspace
    // root, one level up.
    let script: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("oracles/ always has a parent")
        .join("linker.ld");

    println!("cargo::rustc-link-arg-bins=-T{}", script.display());
    println!("cargo::rerun-if-changed={}", script.display());
}
