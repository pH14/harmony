// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::error::VmStateError;
use crate::types::{TimerQueueState, VtimeState};
use crate::{ARCH_X86_64, VmState};

pub trait SnapshotRecords: Sized {
    const ARCH_TAG: u16;

    fn encode(&self) -> Result<Vec<u8>, VmStateError>;

    fn decode(bytes: &[u8]) -> Result<Self, VmStateError>;

    fn vtime(&self) -> &VtimeState;

    fn timers(&self) -> &TimerQueueState;

    fn entropy_bytes(&self) -> &[u8];
}

impl SnapshotRecords for VmState {
    const ARCH_TAG: u16 = ARCH_X86_64;

    fn encode(&self) -> Result<Vec<u8>, VmStateError> {
        VmState::encode(self)
    }

    fn decode(bytes: &[u8]) -> Result<Self, VmStateError> {
        VmState::decode(bytes)
    }

    fn vtime(&self) -> &VtimeState {
        &self.vtime
    }

    fn timers(&self) -> &TimerQueueState {
        &self.timers
    }

    fn entropy_bytes(&self) -> &[u8] {
        &self.hypercall
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trait_codec_is_the_inherent_codec() {
        let mut s = VmState {
            contract_hash: [9u8; 32],
            hypercall: vec![1, 2, 3],
            ..Default::default()
        };
        s.vtime.snapshot_vns = 42;

        let via_trait = <VmState as SnapshotRecords>::encode(&s).unwrap();
        assert_eq!(via_trait, VmState::encode(&s).unwrap());
        let back = <VmState as SnapshotRecords>::decode(&via_trait).unwrap();
        assert_eq!(back, s);

        assert_eq!(<VmState as SnapshotRecords>::ARCH_TAG, ARCH_X86_64);
        assert_eq!(s.vtime(), &s.vtime);
        assert_eq!(s.timers(), &s.timers);
        assert_eq!(s.entropy_bytes(), &s.hypercall[..]);
    }
}
