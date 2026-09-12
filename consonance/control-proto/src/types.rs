// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Reproducer {
    pub blob_version: u16,
    pub bytes: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Answer(pub Vec<u8>);

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HostFault(pub Vec<u8>);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Moment(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct SnapId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct DecisionId(pub u64);

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Resolution {
    pub vtime: Moment,
    pub service: u16,
    pub id: DecisionId,
    pub answer: Answer,
}

pub mod class_bit {
    pub const ENTROPY: u16 = 1;
    pub const PAYLOAD: u16 = 2;
    pub const SCHEDULER: u16 = 3;
    pub const NET_SEND: u16 = 4;
    pub const BLOCK_IO: u16 = 5;
    pub const PROCESS: u16 = 6;
    pub const BUGGIFY: u16 = 7;
    pub const SNAPSHOT_POINT: u16 = 8;
    pub const ASSERTION: u16 = 9;
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct StopMask(pub u32);

impl StopMask {
    pub const NONE: Self = StopMask(0);

    #[must_use]
    pub fn arm(self, class_bit: u16) -> Self {
        match 1u32.checked_shl(u32::from(class_bit)) {
            Some(bit) => StopMask(self.0 | bit),
            None => self,
        }
    }

    pub fn armed(&self, class_bit: u16) -> bool {
        match 1u32.checked_shl(u32::from(class_bit)) {
            Some(bit) => self.0 & bit != 0,
            None => false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct StopConditions {
    pub deadline: Option<Moment>,
    pub on: StopMask,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum HashScope {
    Whole,
    Disk,
    Region { base: u64, len: u64 },
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Request {
    Hello(Caps),
    Snapshot,
    Drop(SnapId),
    Branch {
        snap: SnapId,
        env: Reproducer,
    },
    Replay(SnapId),
    Run {
        until: StopConditions,
        resolve: Option<Resolution>,
    },
    Hash {
        scope: HashScope,
    },
    Perturb {
        fault: HostFault,
        at: Moment,
    },
    SdkEvents {
        offset: u32,
    },
    Console {
        offset: u32,
    },
    Read {
        gpa: u64,
        len: u32,
    },
    Regs,
    Exec {
        cmd: String,
        deadline: Moment,
    },
    RecordedEnv,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Reply {
    Hello(Caps),
    Unit,
    Stop(StopReason),
    Hash([u8; 32]),
    SdkEvents(Vec<(u64, u32, Vec<u8>)>),
    Console {
        total: u32,
        chunk: Vec<u8>,
    },
    Bytes(Vec<u8>),
    Regs(RegsView),
    ExecResult {
        output: Vec<u8>,
        ok: bool,
    },
    Snapshot {
        id: SnapId,
        at: Moment,
        sdk_events: u64,
        tainted: bool,
    },
    Recorded(Reproducer),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RegsView {
    pub version: u16,
    pub gpr: [u64; 16],
    pub rip: u64,
    pub rflags: u64,
    pub seg: [u16; 6],
    pub cr0: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub moment: Moment,
    pub vtime: u64,
}

impl RegsView {
    pub const VERSION: u16 = 1;
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum StopReason {
    Deadline {
        vtime: Moment,
    },
    Quiescent {
        vtime: Moment,
    },
    Crash {
        vtime: Moment,
        info: CrashInfo,
    },
    Decision {
        vtime: Moment,
        id: DecisionId,
        ctx: Vec<u8>,
    },
    SnapshotPoint {
        vtime: Moment,
    },
    Assertion {
        vtime: Moment,
        ev: EventRef,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CrashKind {
    Panic,
    UnrecoverableFault,
    Shutdown,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CrashInfo {
    pub kind: CrashKind,
    pub detail: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EventRef {
    pub id: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Caps {
    pub protocol_version: u16,
    pub env_version_min: u16,
    pub env_version_max: u16,
    pub coverage: CoverageGeometry,
    pub flags: CapFlags,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct CoverageGeometry {
    pub map_bytes: u32,
    pub producer: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct CapFlags(pub u32);

impl CapFlags {
    pub const NONE: Self = CapFlags(0);
    pub const GUEST_HAS_SDK: Self = CapFlags(1);

    pub fn contains(self, other: CapFlags) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub fn with(self, other: CapFlags) -> Self {
        CapFlags(self.0 | other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_mask_arm_sets_one_shifted_bit() {
        for cb in [
            class_bit::ENTROPY,
            class_bit::PAYLOAD,
            class_bit::SCHEDULER,
            class_bit::NET_SEND,
            class_bit::BLOCK_IO,
            class_bit::PROCESS,
        ] {
            let m = StopMask::NONE.arm(cb);
            assert_eq!(m.0, 1u32 << cb);
            assert!(m.armed(cb));
            for other in 0u16..32 {
                if other != cb {
                    assert!(!m.armed(other));
                }
            }
        }
    }

    #[test]
    fn stop_mask_arm_is_idempotent_and_composes() {
        let m = StopMask::NONE
            .arm(class_bit::BLOCK_IO)
            .arm(class_bit::NET_SEND)
            .arm(class_bit::BLOCK_IO);
        assert!(m.armed(class_bit::BLOCK_IO));
        assert!(m.armed(class_bit::NET_SEND));
        assert!(!m.armed(class_bit::ENTROPY));
        assert_eq!(
            m.0,
            (1u32 << class_bit::BLOCK_IO) | (1u32 << class_bit::NET_SEND)
        );
    }

    #[test]
    fn stop_mask_out_of_range_class_is_a_total_noop() {
        for cb in [32u16, 33, 100, u16::MAX] {
            assert_eq!(StopMask::NONE.arm(cb), StopMask::NONE);
            assert!(!StopMask::NONE.arm(class_bit::BLOCK_IO).armed(cb));
        }
        assert!(!StopMask(u32::MAX).armed(32));
    }

    #[test]
    fn cap_flags_contains_and_with() {
        assert!(CapFlags::GUEST_HAS_SDK.contains(CapFlags::GUEST_HAS_SDK));
        assert!(CapFlags::GUEST_HAS_SDK.contains(CapFlags::NONE));
        assert!(!CapFlags::NONE.contains(CapFlags::GUEST_HAS_SDK));
        let both = CapFlags(0b10).with(CapFlags::GUEST_HAS_SDK);
        assert!(both.contains(CapFlags::GUEST_HAS_SDK));
        assert!(both.contains(CapFlags(0b10)));
        assert_eq!(both.0, 0b11);
        assert_eq!(
            CapFlags::GUEST_HAS_SDK.with(CapFlags::GUEST_HAS_SDK),
            CapFlags::GUEST_HAS_SDK,
            "with is a set-union, not XOR"
        );
        assert_eq!(CapFlags(0b11).with(CapFlags(0b01)), CapFlags(0b11));
    }
}
