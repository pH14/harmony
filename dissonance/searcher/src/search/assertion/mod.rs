// SPDX-License-Identifier: AGPL-3.0-or-later

mod catalog;
mod compile;
mod expr;
mod key;
mod strategy;

pub use catalog::{
    Assertion, Catalog, Declared, EACH_FIELDS, NONE, Number, RECORD_SLOTS, Record, Signal,
};
pub use compile::{Accumulator, Compiled, KeyView, MAX_AGGREGATES, OBJECTIVE_WIDTH, Observed};
pub use key::{AssertionKey, MAX_PREFERENCES, Wide};
pub use strategy::{Lines, Shared, StageLines, Steps, Strategy};

#[cfg(test)]
mod tests;
