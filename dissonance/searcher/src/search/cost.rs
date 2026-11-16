// SPDX-License-Identifier: AGPL-3.0-or-later

use std::ops::Add;

use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
pub struct ShrinkCost {
    pub primary: u64,
    pub secondary: u64,
}

impl ShrinkCost {
    pub const ZERO: Self = Self {
        primary: 0,
        secondary: 0,
    };

    #[must_use]
    pub const fn new(primary: u64, secondary: u64) -> Self {
        Self { primary, secondary }
    }

    #[must_use]
    pub const fn saving_over(self, cheaper: Self) -> (u64, u64) {
        (
            self.primary.saturating_sub(cheaper.primary),
            self.secondary.saturating_sub(cheaper.secondary),
        )
    }
}

impl Add for ShrinkCost {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            primary: self.primary.saturating_add(other.primary),
            secondary: self.secondary.saturating_add(other.secondary),
        }
    }
}
