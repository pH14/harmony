// SPDX-License-Identifier: AGPL-3.0-or-later

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(harmony_omit_nested_state)");
}
