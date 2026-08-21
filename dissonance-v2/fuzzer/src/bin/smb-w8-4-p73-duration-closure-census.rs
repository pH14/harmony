// SPDX-License-Identifier: AGPL-3.0-or-later

//! Entry point for the sealed World 8-4 p73 source-mask duration-closure census.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    fuzzer::smb::duration_closure_census::run_from_process(
        include_bytes!("smb-w8-4-p73-duration-closure-census.rs"),
        include_bytes!("../smb/duration_closure_census.rs"),
    )
}
