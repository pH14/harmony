// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bounded action alphabet shared by the host campaign and guest agent.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Version of the package-owned eight-byte action record.
///
/// Version two adds the persistent `DelayForward` catalog member.  Keeping
/// the version in the record prevents an older guest fallback from silently
/// interpreting that index as an unrelated action.
pub const ACTION_SCHEMA_VERSION: u16 = 2;

/// Fixed latency used by the bounded delay action.  It is part of the package
/// contract because the eight-byte action record carries only the catalog
/// index and logical work windows.
pub const DELAY_FORWARD_LATENCY_MS: u32 = 5;

/// Operations in the bounded fault catalog.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u16)]
pub enum CatalogAction {
    /// Advance the deterministic supervisor clock without changing topology.
    Advance = 0,
    /// Install a partition from the first node to the second node.
    PartitionForward = 1,
    /// Recover the most recently installed directional network fault.
    RecoverForward = 2,
    /// Pause and then resume the second node around a work window.
    PauseReplica = 3,
    /// Restart the second node around a work window.
    RestartReplica = 4,
    /// Add a persistent delay from the first node to the second node.
    /// `RecoverForward` removes it after any pending work has settled.
    DelayForward = 5,
}

impl CatalogAction {
    pub const CATALOG_LEN: u16 = 6;

    #[must_use]
    pub fn from_index(index: u16) -> Option<Self> {
        match index {
            0 => Some(Self::Advance),
            1 => Some(Self::PartitionForward),
            2 => Some(Self::RecoverForward),
            3 => Some(Self::PauseReplica),
            4 => Some(Self::RestartReplica),
            5 => Some(Self::DelayForward),
            _ => None,
        }
    }

    #[must_use]
    pub const fn index(self) -> u16 {
        self as u16
    }
}

/// One deterministic search action.
///
/// The wire representation is little-endian `[version, catalog, work,
/// recovery]`, four `u16`s, so the guest can decode it without depending on
/// the host searcher or Rust's enum layout.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FaultAction {
    pub catalog: u16,
    pub work_ticks: u16,
    pub recovery_ticks: u16,
}

impl FaultAction {
    pub const WIRE_LEN: usize = 8;

    #[must_use]
    pub const fn new(catalog: CatalogAction, work_ticks: u16, recovery_ticks: u16) -> Self {
        Self {
            catalog: catalog.index(),
            work_ticks,
            recovery_ticks,
        }
    }

    #[must_use]
    pub fn kind(self) -> Option<CatalogAction> {
        CatalogAction::from_index(self.catalog)
    }

    #[must_use]
    pub fn encode(self) -> [u8; Self::WIRE_LEN] {
        let mut bytes = [0_u8; Self::WIRE_LEN];
        bytes[0..2].copy_from_slice(&ACTION_SCHEMA_VERSION.to_le_bytes());
        bytes[2..4].copy_from_slice(&self.catalog.to_le_bytes());
        bytes[4..6].copy_from_slice(&self.work_ticks.to_le_bytes());
        bytes[6..8].copy_from_slice(&self.recovery_ticks.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ActionDecodeError> {
        if bytes.len() != Self::WIRE_LEN {
            return Err(ActionDecodeError::Length(bytes.len()));
        }
        let version = u16::from_le_bytes([bytes[0], bytes[1]]);
        if version != ACTION_SCHEMA_VERSION {
            return Err(ActionDecodeError::Version(version));
        }
        let action = Self {
            catalog: u16::from_le_bytes([bytes[2], bytes[3]]),
            work_ticks: u16::from_le_bytes([bytes[4], bytes[5]]),
            recovery_ticks: u16::from_le_bytes([bytes[6], bytes[7]]),
        };
        if action.kind().is_none() {
            return Err(ActionDecodeError::Catalog(action.catalog));
        }
        Ok(action)
    }
}

impl fmt::Display for FaultAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}(work={}, recovery={})",
            self.kind().map_or("unknown", CatalogAction::name),
            self.work_ticks,
            self.recovery_ticks
        )
    }
}

impl CatalogAction {
    const fn name(self) -> &'static str {
        match self {
            Self::Advance => "advance",
            Self::PartitionForward => "partition-forward",
            Self::RecoverForward => "recover-forward",
            Self::PauseReplica => "pause-replica",
            Self::RestartReplica => "restart-replica",
            Self::DelayForward => "delay-forward",
        }
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum ActionDecodeError {
    #[error("fault action has length {0}, expected 8")]
    Length(usize),
    #[error("fault action schema version {0} is unsupported")]
    Version(u16),
    #[error("fault action catalog index {0} is outside the bounded catalog")]
    Catalog(u16),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_wire_is_fixed_and_round_trips() {
        let action = FaultAction::new(CatalogAction::PartitionForward, 7, 11);
        assert_eq!(action.encode().len(), FaultAction::WIRE_LEN);
        assert_eq!(FaultAction::decode(&action.encode()), Ok(action));
    }

    #[test]
    fn action_wire_rejects_version_length_and_catalog_drift() {
        let action = FaultAction::new(CatalogAction::Advance, 1, 1);
        let mut bytes = action.encode();
        bytes[0..2].copy_from_slice(&(ACTION_SCHEMA_VERSION + 1).to_le_bytes());
        assert!(matches!(
            FaultAction::decode(&bytes),
            Err(ActionDecodeError::Version(3))
        ));
        assert!(matches!(
            FaultAction::decode(&bytes[..7]),
            Err(ActionDecodeError::Length(7))
        ));
        let mut bytes = action.encode();
        bytes[0..2].copy_from_slice(&ACTION_SCHEMA_VERSION.to_le_bytes());
        bytes[2..4].copy_from_slice(&99_u16.to_le_bytes());
        assert!(matches!(
            FaultAction::decode(&bytes),
            Err(ActionDecodeError::Catalog(99))
        ));
    }

    #[test]
    fn delay_catalog_member_is_wire_stable_and_persistent_by_contract() {
        let action = FaultAction::new(CatalogAction::DelayForward, 3, 4);
        assert_eq!(CatalogAction::CATALOG_LEN, 6);
        assert_eq!(
            CatalogAction::from_index(5),
            Some(CatalogAction::DelayForward)
        );
        assert_eq!(FaultAction::decode(&action.encode()), Ok(action));
        assert_eq!(DELAY_FORWARD_LATENCY_MS, 5);
    }
}
