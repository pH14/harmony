// SPDX-License-Identifier: AGPL-3.0-or-later

mod state;

pub use state::{
    ARM64_GIC_BITMAP_WORDS, ARM64_GIC_PRIORITY_BYTES, Arm64CoreRegs, Arm64DebugState,
    Arm64GicState, Arm64InterruptState, Arm64SimdFpState, Arm64SysregFile, Arm64VcpuState,
    Arm64VtimerState,
};
pub(crate) use state::{canonicalize_core_regs, has_noncanonical_core_regs};

use crate::arch::{Arch, ArchExit};
use crate::exit::ExitReason;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64;

impl Arch for Arm64 {
    type Exit = Arm64Exit;
    type Injection = Arm64Injection;
    type VcpuState = Arm64VcpuState;
    type Policy = Arm64Policy;
    type IntId = GicIntId;
    type Caps = Arm64Caps;
    type Completion = Arm64Completion;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arm64Exit {
    Sysreg { sysreg: u32, write: Option<u64> },
}

impl ArchExit for Arm64Exit {
    fn reason(&self) -> ExitReason {
        match self {
            Arm64Exit::Sysreg { .. } => ExitReason::Sysreg,
        }
    }

    fn stages_completion(&self) -> bool {
        match self {
            Arm64Exit::Sysreg { .. } => true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(transparent)]
pub struct GicIntId(pub u32);

impl GicIntId {
    pub const SPURIOUS: GicIntId = GicIntId(1023);
    pub const MAX_SPI: u32 = 1019;

    pub fn is_sgi(self) -> bool {
        self.0 < 16
    }

    pub fn is_ppi(self) -> bool {
        (16..32).contains(&self.0)
    }

    pub fn is_spi(self) -> bool {
        (32..=Self::MAX_SPI).contains(&self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arm64Injection {
    Interrupt { intid: GicIntId },
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Arm64Policy {
    pub id_regs: IdRegModel,
    pub sysreg_traps: SysregTrapPolicy,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct IdRegModel {
    pub regs: std::collections::BTreeMap<u32, u64>,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SysregTrapPolicy {
    pub trapped: std::collections::BTreeSet<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Arm64Caps {
    pub in_kernel_gic: bool,
    pub asid_bits: Arm64AsidBits,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Arm64AsidBits {
    #[default]
    Eight,
    Sixteen,
}

impl Arm64AsidBits {
    pub const ID_AA64MMFR0_EL1: u32 = 0xc038;
    const FIELD_SHIFT: u32 = 4;
    const FIELD_MASK: u64 = 0xf << Self::FIELD_SHIFT;

    pub fn from_id_register(value: u64) -> Option<Self> {
        match (value & Self::FIELD_MASK) >> Self::FIELD_SHIFT {
            0b0000 => Some(Self::Eight),
            0b0010 => Some(Self::Sixteen),
            _ => None,
        }
    }

    pub fn apply_to_id_register(self, value: u64) -> u64 {
        let field = match self {
            Self::Eight => 0b0000,
            Self::Sixteen => 0b0010,
        };
        (value & !Self::FIELD_MASK) | (field << Self::FIELD_SHIFT)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arm64Completion {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asid_bits_round_trip_through_the_id_register() {
        assert_eq!(
            Arm64AsidBits::from_id_register(0x0000_0111_0f10_0002),
            Some(Arm64AsidBits::Eight)
        );
        assert_eq!(
            Arm64AsidBits::from_id_register(0x0000_0111_0f10_0022),
            Some(Arm64AsidBits::Sixteen)
        );
        assert_eq!(Arm64AsidBits::from_id_register(0x10), None);
        assert_eq!(
            Arm64AsidBits::Sixteen.apply_to_id_register(0x0000_0111_0f10_0002),
            0x0000_0111_0f10_0022
        );
        assert_eq!(
            Arm64AsidBits::Eight.apply_to_id_register(0x0000_0111_0f10_0022),
            0x0000_0111_0f10_0002
        );
    }

    #[test]
    fn sysreg_exit_reason_and_completion_staging() {
        let read = Arm64Exit::Sysreg {
            sysreg: 0x0001_2345,
            write: None,
        };
        let write = Arm64Exit::Sysreg {
            sysreg: 0x0001_2345,
            write: Some(7),
        };
        assert_eq!(read.reason(), ExitReason::Sysreg);
        assert_eq!(write.reason(), ExitReason::Sysreg);
        assert!(read.stages_completion());
        assert!(write.stages_completion());
    }

    #[test]
    fn gic_intid_classes_are_the_gicv3_identity_space() {
        assert!(GicIntId(0).is_sgi());
        assert!(GicIntId(15).is_sgi());
        assert!(!GicIntId(16).is_sgi());
        assert!(GicIntId(16).is_ppi());
        assert!(GicIntId(31).is_ppi());
        assert!(!GicIntId(32).is_ppi());
        assert!(GicIntId(32).is_spi());
        assert!(GicIntId(1019).is_spi());
        assert!(!GicIntId(1020).is_spi());
        assert_eq!(GicIntId::SPURIOUS, GicIntId(1023));
    }
}
