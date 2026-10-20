// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

#[derive(Default, Serialize)]
pub(super) struct ExpansionProfile {
    pub reserve_ns: u128,
    pub selection_ns: u128,
    pub origin_ns: u128,
    pub complete_ns: u128,
    pub admit_ns: u128,
    pub validation_ns: u128,
    pub evidence_ns: u128,
    pub archive_ns: u128,
    pub maintenance_ns: u128,
    pub watch_ns: u128,
    pub record_ns: u128,
    pub stream_ns: u128,
    pub progress_ns: u128,
    pub checkpoint_ns: u128,
}

pub(super) fn record_profile(total: &mut u128, started: Option<Moment>) {
    if let Some(started) = started {
        *total = total.saturating_add(started.elapsed().as_nanos());
    }
}
