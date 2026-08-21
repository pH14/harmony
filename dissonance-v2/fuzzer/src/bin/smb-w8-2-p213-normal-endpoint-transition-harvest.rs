// SPDX-License-Identifier: AGPL-3.0-or-later

//! Temporary standalone entry point for the sealed World 8-2 p213 transition harvest.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    fuzzer::smb::endpoint_frontier_harvest::run_from_process(
        include_bytes!("smb-w8-2-p213-normal-endpoint-transition-harvest.rs"),
        include_bytes!("../smb/endpoint_frontier_harvest.rs"),
    )
}
