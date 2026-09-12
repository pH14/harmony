// SPDX-License-Identifier: AGPL-3.0-or-later

use thiserror::Error;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Error)]
pub enum EnvError {
    #[error("unsupported blob version {0}")]
    BadVersion(u16),
    #[error("malformed environment blob")]
    Malformed,
    #[error("environment composition offset overflowed the Moment axis")]
    Overflow,
    #[error("unsupported environment composition")]
    UnsupportedComposition,
}
