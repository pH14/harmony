// SPDX-License-Identifier: AGPL-3.0-or-later

mod catalog;
mod checks;
mod compile;
mod expr;
mod key;
mod records;
mod rekey;
mod strategy;

pub use catalog::{
    Assertion, Catalog, Declarations, Declared, EACH_FIELDS, NONE, Number, RECORD_SLOTS, Record,
    RecordWriter, Signal,
};
pub use checks::{Checker, Finding, THIN_TIER_STATES};
pub use compile::{Accumulator, Compiled, KeyView, MAX_AGGREGATES, OBJECTIVE_WIDTH, Observed};
pub use key::{AssertionKey, MAX_PREFERENCES, Wide};
pub use records::{
    RECORDS_FORMAT, RecordedAction, Recording, RecordsHeader, RecordsSize, RecordsWriter,
    StreamIdentity,
};
pub use rekey::{Holders, RecordedJob, Rekeyed, Rekeying, Tier, same_decisions};
pub use strategy::{Lines, Shared, StageLines, Steps, Strategy};

#[cfg(test)]
mod tests;
