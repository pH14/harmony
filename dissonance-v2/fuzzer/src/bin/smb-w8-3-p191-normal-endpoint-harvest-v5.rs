// SPDX-License-Identifier: AGPL-3.0-or-later

//! Temporary entry point for the final sealed World 8-3 p191 endpoint harvest.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    fuzzer::smb::endpoint_frontier_harvest::run_from_process(
        include_bytes!("smb-w8-3-p191-normal-endpoint-harvest-v5.rs"),
        include_bytes!("../smb/endpoint_frontier_harvest.rs"),
    )
}
