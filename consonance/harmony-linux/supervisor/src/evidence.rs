// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CheckEvidence {
    pub run: u64,
    pub start_generation: u64,
    pub end_generation: u64,
    pub pid: u64,
}
