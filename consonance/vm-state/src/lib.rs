// SPDX-License-Identifier: AGPL-3.0-or-later

mod arm64;
mod codec;
mod error;
mod records;
mod types;
mod wire;

pub use arm64::{
    Arm64Debug, Arm64Interrupts, Arm64Regs, Arm64SimdFp, Arm64Sysregs, Arm64VmState, Arm64Vtimer,
};
pub use error::VmStateError;
pub use records::SnapshotRecords;
pub use types::{
    DebugRegs, DeviceBlob, MpState, MsrBlock, Segment, TimerEntry, TimerQueueState, VcpuEvents,
    VcpuRegs, VcpuSregs, VtimeState, Xcrs, XsaveImage,
};

pub const VM_STATE_MAGIC: u32 = 0x3153_4D56;

pub const VM_STATE_VERSION: u16 = 3;

pub const ARCH_X86_64: u16 = 1;

pub const ARCH_AARCH64: u16 = 2;

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct VmState {
    pub regs: VcpuRegs,
    pub sregs: VcpuSregs,
    pub xcrs: Xcrs,
    pub debugregs: DebugRegs,
    pub events: VcpuEvents,
    pub mp_state: MpState,
    pub msrs: MsrBlock,
    pub xsave: XsaveImage,
    pub vtime: VtimeState,
    pub timers: TimerQueueState,
    pub hypercall: Vec<u8>,
    pub devices: DeviceBlob,
    pub contract_hash: [u8; 32],
}
