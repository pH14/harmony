// SPDX-License-Identifier: MIT OR Apache-2.0
#![no_std]
#![recursion_limit = "1000"]
#![allow(warnings)]
include!(concat!(
    env!("OUT_DIR"),
    "/wasmi-0.46.0/src/harmony_root.rs"
));

pub const HARMONY_COMPILER: &str = env!("HARMONY_WASMI_COMPILER");

mod harmony_wasm;
