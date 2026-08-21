// SPDX-License-Identifier: AGPL-3.0-or-later

//! Temporary entry point for the sealed World 8-4 p73 midpoint-compaction canary.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    fuzzer::smb::endpoint_frontier_harvest::run_from_process(
        include_bytes!("smb-w8-4-p73-midpoint-compaction-canary.rs"),
        include_bytes!("../smb/endpoint_frontier_harvest.rs"),
    )
}
