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
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arm64Completion {}

#[cfg(test)]
mod tests {
    use super::*;

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
