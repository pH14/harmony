// SPDX-License-Identifier: AGPL-3.0-or-later

//! Temporary standalone entry point for the sealed p305 endpoint frontier harvest.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    fuzzer::smb::endpoint_frontier_harvest::run_from_process(
        include_bytes!("smb-p305-endpoint-frontier-harvest.rs"),
        include_bytes!("../smb/endpoint_frontier_harvest.rs"),
    )
}
