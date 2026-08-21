// SPDX-License-Identifier: AGPL-3.0-or-later

//! Entry point for the sealed World 8-4 p153 pinned-window novel-mask harvest.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    fuzzer::smb::endpoint_frontier_harvest::run_from_process(
        include_bytes!("smb-w8-4-p153-pinned-window-novel-mask-harvest.rs"),
        include_bytes!("../smb/endpoint_frontier_harvest.rs"),
    )
}
