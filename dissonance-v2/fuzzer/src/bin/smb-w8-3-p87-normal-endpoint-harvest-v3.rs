// SPDX-License-Identifier: AGPL-3.0-or-later

//! Temporary standalone entry point for the sealed World 8-3 p87 endpoint harvest.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    fuzzer::smb::endpoint_frontier_harvest::run_from_process(
        include_bytes!("smb-w8-3-p87-normal-endpoint-harvest-v3.rs"),
        include_bytes!("../smb/endpoint_frontier_harvest.rs"),
    )
}
