// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sealed paired observer-prefix archive-admission canary.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    fuzzer::smb::prefix_admission_canary::run_from_process(
        include_bytes!("smb-prefix-admission-canary.rs"),
        include_bytes!("../smb/prefix_admission_canary.rs"),
    )
}
