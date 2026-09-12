// SPDX-License-Identifier: AGPL-3.0-or-later

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VtimeError {
    #[error("invalid periodic timer: period is zero")]
    ZeroPeriod,
}
