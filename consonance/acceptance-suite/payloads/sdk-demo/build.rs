// SPDX-License-Identifier: AGPL-3.0-or-later
fn main() {
    // CARGO_MANIFEST_DIR is always set by cargo for build scripts.
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/../linker.ld");
    println!("cargo:rerun-if-changed={dir}/../linker.ld");
}
