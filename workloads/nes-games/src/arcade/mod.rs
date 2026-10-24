// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod castlevania;
pub mod famidash;
pub mod feed;
pub mod mega_man;
pub mod metroid;
pub mod pac_man;
pub mod punchout;
pub mod smb;
pub mod trail;
pub mod twin_dragons;
pub mod zelda;

use std::{error::Error, sync::Arc};

use machine::{
    MachineError,
    nes::{ButtonChord, WRAM_SIZE},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::search::{campaign::Workload, draw::DrawMixture};

pub const HIDDEN: u16 = u16::MAX;

pub type Machines<M> = Arc<dyn Fn() -> Result<M, MachineError> + Send + Sync>;

pub trait ArcadeGame:
    Workload<Action = ButtonChord, ArchiveReport: Serialize> + Sized + 'static
{
    type Machine;
    type View: Serialize + DeserializeOwned + Copy + Send + 'static;

    const MEMORY_BYTES: usize = WRAM_SIZE;

    fn arcade(
        rom: &[u8],
        identity: &str,
        identity_sha256: &str,
        machines: Machines<Self::Machine>,
    ) -> Self;

    fn arcade_from(
        rom: &[u8],
        identity: &str,
        identity_sha256: &str,
        machines: Machines<Self::Machine>,
        _level: u8,
    ) -> Self {
        Self::arcade(rom, identity, identity_sha256, machines)
    }

    fn run() -> Self::Run;

    fn mixture() -> DrawMixture;

    fn motion(wram: &[u8; WRAM_SIZE]) -> [u16; 4];

    fn memory_cleared(memory: &[u8]) -> u8;

    fn memory_view(memory: &[u8]) -> Result<Self::View, Box<dyn Error>>;

    fn view(observation: &Self::Observations) -> Self::View;

    fn frame_count(observation: &Self::Observations) -> u64;

    fn cleared(observation: &Self::Observations) -> u8;

    fn emulator_state(snapshot: &Self::Snapshot) -> &[u8];

    fn snapshot_frame_count(snapshot: &Self::Snapshot) -> u64;

    fn probe() -> trail::Probe {
        trail::Probe {
            motion: Self::motion,
            cleared: Self::memory_cleared,
            memory_bytes: Self::MEMORY_BYTES,
        }
    }
}

#[must_use]
pub fn snapshot_digest(bytes: &[u8]) -> u64 {
    let mut a: u32 = 0x811c_9dc5;
    let mut b: u32 = 0x01c9_3a75;
    for &byte in bytes {
        a = (a ^ u32::from(byte)).wrapping_mul(0x0100_0193);
        b = (b ^ u32::from(byte))
            .wrapping_mul(0x0100_0193)
            .rotate_left(5);
    }
    (u64::from(a) << 32) | u64::from(b)
}

#[cfg(test)]
mod tests {
    use super::snapshot_digest;

    #[test]
    fn snapshot_digest_matches_the_browser_reference() {
        assert_eq!(snapshot_digest(&[]), 0x811c_9dc5_01c9_3a75);
        assert_eq!(snapshot_digest(b"twin"), 0x9a87_04f3_35dd_7e16);
        assert_ne!(snapshot_digest(b"twin"), snapshot_digest(b"twio"));
    }
}
