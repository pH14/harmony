// SPDX-License-Identifier: AGPL-3.0-or-later

//! Entry point for the sealed World 8-4 p153 regression-bridge H8 harvest.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    fuzzer::smb::regression_bridge_harvest::run_from_process(
        include_bytes!("smb-w8-4-p153-regression-bridge-h8-harvest.rs"),
        include_bytes!("../smb/regression_bridge_harvest.rs"),
    )
}
