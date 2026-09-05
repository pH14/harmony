// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **arm64 record set** ([`Arm64VmState`]) — the second implementor of
//! [`SnapshotRecords`](crate::SnapshotRecords), under
//! [`ARCH_AARCH64`](crate::ARCH_AARCH64) in the same TLV container.
//!
//! **A minimal, skeleton record set** (`tasks/112` M1): the core registers, a
//! small named EL1 system-register file, and the arch-neutral engine blocks —
//! enough to encode/decode a trivial vCPU state and round-trip it through the
//! container. **Which sysregs an arm64 snapshot must carry is M4's measured
//! decision** (`docs/DETERMINISM.md`); the full record set is
//! `TODO(AA-6)` and
//! lands under a bumped section layout, never guessed here.
//! designed-not-frozen (AA-3).
//!
//! The section tags below are the *arm64* record set's own tag space — tags
//! are meaningful only under this container arch tag, exactly why the v2
//! header carries one (`docs/ARCHITECTURE.md`). A blob with a foreign
//! arch tag is rejected loudly ([`VmStateError::UnsupportedArch`]), never
//! reinterpreted.

use zerocopy::little_endian::U64;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::codec::{
    Reader, decode_contract_hash, decode_mp_state, decode_timers, encode_mp_state, encode_timers,
    put_section, read_fixed,
};
use crate::error::VmStateError;
use crate::records::SnapshotRecords;
use crate::types::{DeviceBlob, MpState, TimerQueueState, VtimeState};
use crate::wire::{HeaderWire, VtimeWire};
use crate::{ARCH_AARCH64, VM_STATE_MAGIC, VM_STATE_VERSION};

// Section tags, in their canonical ascending order. Every arm64 blob carries
// all of them exactly once; there are no optional sections.
const TAG_REGS: u16 = 1;
const TAG_SYSREGS: u16 = 2;
const TAG_MP_STATE: u16 = 3;
const TAG_VTIME: u16 = 4;
const TAG_TIMERS: u16 = 5;
const TAG_HYPERCALL: u16 = 6;
const TAG_DEVICES: u16 = 7;
const TAG_CONTRACT_HASH: u16 = 8;
const TAG_SIMD_FP: u16 = 9;
const TAG_DEBUG: u16 = 10;
const TAG_VTIMER: u16 = 11;
const TAG_INTERRUPTS: u16 = 12;

/// The number of sections every arm64 blob carries.
const SECTION_COUNT: u16 = 12;

/// Length of the fixed container header (shared with the x86 record set).
const HEADER_LEN: usize = 10;

/// The complete non-memory arm64 machine snapshot (skeleton record set).
///
/// The vmm-core arm64 vendor fills this from the live machine; this crate
/// encodes it ([`Arm64VmState::encode`]) and decodes it back
/// ([`Arm64VmState::decode`]). Equal values encode to identical bytes.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Arm64VmState {
    /// Core registers (`x0..x30`, `SP`, `PC`, `PSTATE`, the EL1 banked
    /// exception registers).
    pub regs: Arm64Regs,
    /// The skeleton EL1 system-register file (full set `TODO(AA-6)`).
    pub sysregs: Arm64Sysregs,
    /// SIMD/FP register file.
    pub simd_fp: Arm64SimdFp,
    /// Debug register file and trap controls.
    pub debug: Arm64Debug,
    /// Virtual-timer register and framework offset/mask state.
    pub vtimer: Arm64Vtimer,
    /// Pending interrupt levels.
    pub interrupts: Arm64Interrupts,
    /// Runnable vs halted (WFI-halted on arm64).
    pub mp_state: MpState,
    /// V-time clock snapshot (`snapshot_vns` + ratio config) — the engine's
    /// arch-neutral block, identical in shape to the x86 record set's.
    pub vtime: VtimeState,
    /// Absolute-V-time timer-queue contents (a vmm-core snapshot always seals
    /// it empty; the fabric timer rides the device blob).
    pub timers: TimerQueueState,
    /// The engine's entropy-stream / hypercall-dispatcher state bytes.
    pub hypercall: Vec<u8>,
    /// The arm64 vendor's device blob (PL011 + GIC state; opaque here).
    pub devices: DeviceBlob,
    /// SHA-256 of the ratified ARM CPU contract this snapshot was taken under
    /// (the contract document is port work / AA-6; the skeleton stamps its
    /// policy-skeleton hash). Compared by vmm-core, not here.
    pub contract_hash: [u8; 32],
}

/// The arm64 core register record — mirrors `vmm-backend`'s `Arm64CoreRegs`
/// as plain data (rule #2: no sibling dependency; consistency by review).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[allow(missing_docs)] // the register names are self-documenting
pub struct Arm64Regs {
    pub x: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
    pub sp_el1: u64,
    pub elr_el1: u64,
    pub spsr_el1: u64,
}

/// The skeleton EL1 system-register record — mirrors `vmm-backend`'s
/// `Arm64SysregFile` (full record set `TODO(AA-6)`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[allow(missing_docs)] // the system-register names are self-documenting
pub struct Arm64Sysregs {
    pub sctlr_el1: u64,
    pub ttbr0_el1: u64,
    pub ttbr1_el1: u64,
    pub tcr_el1: u64,
    pub mair_el1: u64,
    pub vbar_el1: u64,
    pub cpacr_el1: u64,
    pub esr_el1: u64,
    pub far_el1: u64,
    pub tpidr_el0: u64,
    pub tpidr_el1: u64,
    pub cntkctl_el1: u64,
}

/// SIMD/FP snapshot record.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64SimdFp {
    /// `Q0..Q31` in architectural byte order.
    pub q: [[u8; 16]; 32],
    /// Floating-point control register.
    pub fpcr: u64,
    /// Floating-point status register.
    pub fpsr: u64,
}

/// Debug snapshot record.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64Debug {
    /// `DBGBVR0_EL1..DBGBVR15_EL1`.
    pub breakpoint_value: [u64; 16],
    /// `DBGBCR0_EL1..DBGBCR15_EL1`.
    pub breakpoint_control: [u64; 16],
    /// `DBGWVR0_EL1..DBGWVR15_EL1`.
    pub watchpoint_value: [u64; 16],
    /// `DBGWCR0_EL1..DBGWCR15_EL1`.
    pub watchpoint_control: [u64; 16],
    /// `MDSCR_EL1`.
    pub mdscr_el1: u64,
    /// Whether guest debug exceptions trap.
    pub trap_debug_exceptions: bool,
    /// Whether guest debug-register accesses trap.
    pub trap_debug_reg_accesses: bool,
}

/// Hypervisor.framework virtual-timer snapshot record.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64Vtimer {
    /// `CNTV_CTL_EL0`.
    pub cntv_ctl_el0: u64,
    /// `CNTV_CVAL_EL0`.
    pub cntv_cval_el0: u64,
    /// Framework automatic-timer-exit mask.
    pub masked: bool,
    /// Framework host-counter offset.
    pub offset: u64,
}

/// Pending interrupt-level snapshot record.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64Interrupts {
    /// Pending IRQ level.
    pub irq: bool,
    /// Pending FIQ level.
    pub fiq: bool,
}

/// `Arm64Regs` on the wire: 37 little-endian `u64`s in declaration order.
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Arm64RegsWire {
    x: [U64; 31],
    sp: U64,
    pc: U64,
    pstate: U64,
    sp_el1: U64,
    elr_el1: U64,
    spsr_el1: U64,
}

impl From<&Arm64Regs> for Arm64RegsWire {
    fn from(r: &Arm64Regs) -> Self {
        Self {
            x: r.x.map(U64::from),
            sp: r.sp.into(),
            pc: r.pc.into(),
            pstate: r.pstate.into(),
            sp_el1: r.sp_el1.into(),
            elr_el1: r.elr_el1.into(),
            spsr_el1: r.spsr_el1.into(),
        }
    }
}

impl From<&Arm64RegsWire> for Arm64Regs {
    fn from(w: &Arm64RegsWire) -> Self {
        Self {
            x: w.x.map(|v| v.get()),
            sp: w.sp.get(),
            pc: w.pc.get(),
            pstate: w.pstate.get(),
            sp_el1: w.sp_el1.get(),
            elr_el1: w.elr_el1.get(),
            spsr_el1: w.spsr_el1.get(),
        }
    }
}

/// `Arm64Sysregs` on the wire: 12 little-endian `u64`s in declaration order.
#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Arm64SysregsWire {
    sctlr_el1: U64,
    ttbr0_el1: U64,
    ttbr1_el1: U64,
    tcr_el1: U64,
    mair_el1: U64,
    vbar_el1: U64,
    cpacr_el1: U64,
    esr_el1: U64,
    far_el1: U64,
    tpidr_el0: U64,
    tpidr_el1: U64,
    cntkctl_el1: U64,
}

impl From<&Arm64Sysregs> for Arm64SysregsWire {
    fn from(s: &Arm64Sysregs) -> Self {
        Self {
            sctlr_el1: s.sctlr_el1.into(),
            ttbr0_el1: s.ttbr0_el1.into(),
            ttbr1_el1: s.ttbr1_el1.into(),
            tcr_el1: s.tcr_el1.into(),
            mair_el1: s.mair_el1.into(),
            vbar_el1: s.vbar_el1.into(),
            cpacr_el1: s.cpacr_el1.into(),
            esr_el1: s.esr_el1.into(),
            far_el1: s.far_el1.into(),
            tpidr_el0: s.tpidr_el0.into(),
            tpidr_el1: s.tpidr_el1.into(),
            cntkctl_el1: s.cntkctl_el1.into(),
        }
    }
}

impl From<&Arm64SysregsWire> for Arm64Sysregs {
    fn from(w: &Arm64SysregsWire) -> Self {
        Self {
            sctlr_el1: w.sctlr_el1.get(),
            ttbr0_el1: w.ttbr0_el1.get(),
            ttbr1_el1: w.ttbr1_el1.get(),
            tcr_el1: w.tcr_el1.get(),
            mair_el1: w.mair_el1.get(),
            vbar_el1: w.vbar_el1.get(),
            cpacr_el1: w.cpacr_el1.get(),
            esr_el1: w.esr_el1.get(),
            far_el1: w.far_el1.get(),
            tpidr_el0: w.tpidr_el0.get(),
            tpidr_el1: w.tpidr_el1.get(),
            cntkctl_el1: w.cntkctl_el1.get(),
        }
    }
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Arm64SimdFpWire {
    q: [[u8; 16]; 32],
    fpcr: U64,
    fpsr: U64,
}

impl From<&Arm64SimdFp> for Arm64SimdFpWire {
    fn from(s: &Arm64SimdFp) -> Self {
        Self {
            q: s.q,
            fpcr: s.fpcr.into(),
            fpsr: s.fpsr.into(),
        }
    }
}

impl From<&Arm64SimdFpWire> for Arm64SimdFp {
    fn from(w: &Arm64SimdFpWire) -> Self {
        Self {
            q: w.q,
            fpcr: w.fpcr.get(),
            fpsr: w.fpsr.get(),
        }
    }
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Arm64DebugWire {
    breakpoint_value: [U64; 16],
    breakpoint_control: [U64; 16],
    watchpoint_value: [U64; 16],
    watchpoint_control: [U64; 16],
    mdscr_el1: U64,
    trap_debug_exceptions: u8,
    trap_debug_reg_accesses: u8,
    reserved: [u8; 6],
}

impl From<&Arm64Debug> for Arm64DebugWire {
    fn from(s: &Arm64Debug) -> Self {
        Self {
            breakpoint_value: s.breakpoint_value.map(U64::from),
            breakpoint_control: s.breakpoint_control.map(U64::from),
            watchpoint_value: s.watchpoint_value.map(U64::from),
            watchpoint_control: s.watchpoint_control.map(U64::from),
            mdscr_el1: s.mdscr_el1.into(),
            trap_debug_exceptions: u8::from(s.trap_debug_exceptions),
            trap_debug_reg_accesses: u8::from(s.trap_debug_reg_accesses),
            reserved: [0; 6],
        }
    }
}

fn decode_bool(value: u8) -> Result<bool, VmStateError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(VmStateError::InvalidField),
    }
}

fn decode_debug(w: &Arm64DebugWire) -> Result<Arm64Debug, VmStateError> {
    if w.reserved != [0; 6] {
        return Err(VmStateError::InvalidField);
    }
    Ok(Arm64Debug {
        breakpoint_value: w.breakpoint_value.map(|v| v.get()),
        breakpoint_control: w.breakpoint_control.map(|v| v.get()),
        watchpoint_value: w.watchpoint_value.map(|v| v.get()),
        watchpoint_control: w.watchpoint_control.map(|v| v.get()),
        mdscr_el1: w.mdscr_el1.get(),
        trap_debug_exceptions: decode_bool(w.trap_debug_exceptions)?,
        trap_debug_reg_accesses: decode_bool(w.trap_debug_reg_accesses)?,
    })
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Arm64VtimerWire {
    cntv_ctl_el0: U64,
    cntv_cval_el0: U64,
    offset: U64,
    masked: u8,
    reserved: [u8; 7],
}

impl From<&Arm64Vtimer> for Arm64VtimerWire {
    fn from(s: &Arm64Vtimer) -> Self {
        Self {
            cntv_ctl_el0: s.cntv_ctl_el0.into(),
            cntv_cval_el0: s.cntv_cval_el0.into(),
            offset: s.offset.into(),
            masked: u8::from(s.masked),
            reserved: [0; 7],
        }
    }
}

fn decode_vtimer(w: &Arm64VtimerWire) -> Result<Arm64Vtimer, VmStateError> {
    if w.reserved != [0; 7] {
        return Err(VmStateError::InvalidField);
    }
    Ok(Arm64Vtimer {
        cntv_ctl_el0: w.cntv_ctl_el0.get(),
        cntv_cval_el0: w.cntv_cval_el0.get(),
        masked: decode_bool(w.masked)?,
        offset: w.offset.get(),
    })
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Arm64InterruptsWire {
    irq: u8,
    fiq: u8,
    reserved: [u8; 6],
}

impl From<&Arm64Interrupts> for Arm64InterruptsWire {
    fn from(s: &Arm64Interrupts) -> Self {
        Self {
            irq: u8::from(s.irq),
            fiq: u8::from(s.fiq),
            reserved: [0; 6],
        }
    }
}

fn decode_interrupts(w: &Arm64InterruptsWire) -> Result<Arm64Interrupts, VmStateError> {
    if w.reserved != [0; 6] {
        return Err(VmStateError::InvalidField);
    }
    Ok(Arm64Interrupts {
        irq: decode_bool(w.irq)?,
        fiq: decode_bool(w.fiq)?,
    })
}

impl Arm64VmState {
    /// Encode to the versioned TLV blob under [`ARCH_AARCH64`]. Deterministic:
    /// equal `Arm64VmState` ⇒ equal bytes.
    ///
    /// # Errors
    ///
    /// - [`VmStateError::InvalidField`] for a timer queue violating the
    ///   canonical-order/unique-token/`seq < next_seq` invariants, or a
    ///   variable-length section exceeding `u32::MAX` bytes.
    pub fn encode(&self) -> Result<Vec<u8>, VmStateError> {
        let mut out = Vec::new();
        out.extend_from_slice(
            HeaderWire {
                magic: VM_STATE_MAGIC.into(),
                version: VM_STATE_VERSION.into(),
                // The record set below is arm64's; the tag says so, so a
                // decoder can never reinterpret it as another architecture's.
                arch: ARCH_AARCH64.into(),
                section_count: SECTION_COUNT.into(),
            }
            .as_bytes(),
        );

        put_section(
            &mut out,
            TAG_REGS,
            Arm64RegsWire::from(&self.regs).as_bytes(),
        )?;
        put_section(
            &mut out,
            TAG_SYSREGS,
            Arm64SysregsWire::from(&self.sysregs).as_bytes(),
        )?;
        put_section(&mut out, TAG_MP_STATE, &[encode_mp_state(self.mp_state)])?;
        put_section(&mut out, TAG_VTIME, VtimeWire::from(&self.vtime).as_bytes())?;
        put_section(&mut out, TAG_TIMERS, &encode_timers(&self.timers)?)?;
        put_section(&mut out, TAG_HYPERCALL, &self.hypercall)?;
        put_section(&mut out, TAG_DEVICES, &self.devices.0)?;
        put_section(&mut out, TAG_CONTRACT_HASH, &self.contract_hash)?;
        put_section(
            &mut out,
            TAG_SIMD_FP,
            Arm64SimdFpWire::from(&self.simd_fp).as_bytes(),
        )?;
        put_section(
            &mut out,
            TAG_DEBUG,
            Arm64DebugWire::from(&self.debug).as_bytes(),
        )?;
        put_section(
            &mut out,
            TAG_VTIMER,
            Arm64VtimerWire::from(&self.vtimer).as_bytes(),
        )?;
        put_section(
            &mut out,
            TAG_INTERRUPTS,
            Arm64InterruptsWire::from(&self.interrupts).as_bytes(),
        )?;

        Ok(out)
    }

    /// Decode a blob produced by [`Arm64VmState::encode`]. Strict and total:
    /// validates magic, version, **arch tag**, section count, ordering, and
    /// every field; never panics on arbitrary input.
    ///
    /// # Errors
    ///
    /// The matching [`VmStateError`] — notably
    /// [`VmStateError::UnsupportedArch`] for a blob whose header names another
    /// record set (e.g. an x86 blob), which must never be reinterpreted.
    pub fn decode(bytes: &[u8]) -> Result<Arm64VmState, VmStateError> {
        let header = HeaderWire::read_from_prefix(bytes)
            .map_err(|_| VmStateError::Truncated)?
            .0;
        let magic = header.magic.get();
        if magic != VM_STATE_MAGIC {
            return Err(VmStateError::BadMagic(magic));
        }
        let version = header.version.get();
        if version != VM_STATE_VERSION {
            return Err(VmStateError::UnsupportedVersion(version));
        }
        let arch = header.arch.get();
        if arch != ARCH_AARCH64 {
            return Err(VmStateError::UnsupportedArch(arch));
        }
        let section_count = header.section_count.get();

        let mut r = Reader::new(&bytes[HEADER_LEN..]);
        let mut last_tag: Option<u16> = None;

        let mut regs = None;
        let mut sysregs = None;
        let mut mp_state = None;
        let mut vtime = None;
        let mut timers = None;
        let mut hypercall = None;
        let mut devices = None;
        let mut contract_hash = None;
        let mut simd_fp = None;
        let mut debug = None;
        let mut vtimer = None;
        let mut interrupts = None;

        for _ in 0..section_count {
            let tag = r.u16()?;
            let len = r.u32()? as usize;
            let payload = r.take(len)?;

            // Strictly ascending tags: equal is a duplicate, smaller is out of
            // order (the same folded comparison the x86 decoder uses).
            if let Some(prev) = last_tag
                && tag <= prev
            {
                return Err(if tag == prev {
                    VmStateError::DuplicateTag(tag)
                } else {
                    VmStateError::SectionOrder(tag)
                });
            }
            last_tag = Some(tag);

            match tag {
                TAG_REGS => regs = Some(Arm64Regs::from(&read_fixed::<Arm64RegsWire>(payload)?)),
                TAG_SYSREGS => {
                    sysregs = Some(Arm64Sysregs::from(&read_fixed::<Arm64SysregsWire>(
                        payload,
                    )?));
                }
                TAG_MP_STATE => mp_state = Some(decode_mp_state(payload)?),
                TAG_VTIME => vtime = Some(VtimeState::from(&read_fixed::<VtimeWire>(payload)?)),
                TAG_TIMERS => timers = Some(decode_timers(payload)?),
                TAG_HYPERCALL => hypercall = Some(payload.to_vec()),
                TAG_DEVICES => devices = Some(DeviceBlob(payload.to_vec())),
                TAG_CONTRACT_HASH => contract_hash = Some(decode_contract_hash(payload)?),
                TAG_SIMD_FP => {
                    simd_fp = Some(Arm64SimdFp::from(&read_fixed::<Arm64SimdFpWire>(payload)?));
                }
                TAG_DEBUG => {
                    debug = Some(decode_debug(&read_fixed::<Arm64DebugWire>(payload)?)?);
                }
                TAG_VTIMER => {
                    vtimer = Some(decode_vtimer(&read_fixed::<Arm64VtimerWire>(payload)?)?);
                }
                TAG_INTERRUPTS => {
                    interrupts = Some(decode_interrupts(&read_fixed::<Arm64InterruptsWire>(
                        payload,
                    )?)?);
                }
                other => return Err(VmStateError::UnknownTag(other)),
            }
        }

        if !r.at_end() {
            return Err(VmStateError::TrailingBytes);
        }

        let vtime = vtime.ok_or(VmStateError::MissingSection(TAG_VTIME))?;
        Ok(Arm64VmState {
            regs: regs.ok_or(VmStateError::MissingSection(TAG_REGS))?,
            sysregs: sysregs.ok_or(VmStateError::MissingSection(TAG_SYSREGS))?,
            simd_fp: simd_fp.ok_or(VmStateError::MissingSection(TAG_SIMD_FP))?,
            debug: debug.ok_or(VmStateError::MissingSection(TAG_DEBUG))?,
            vtimer: vtimer.ok_or(VmStateError::MissingSection(TAG_VTIMER))?,
            interrupts: interrupts.ok_or(VmStateError::MissingSection(TAG_INTERRUPTS))?,
            mp_state: mp_state.ok_or(VmStateError::MissingSection(TAG_MP_STATE))?,
            vtime,
            timers: timers.ok_or(VmStateError::MissingSection(TAG_TIMERS))?,
            hypercall: hypercall.ok_or(VmStateError::MissingSection(TAG_HYPERCALL))?,
            devices: devices.ok_or(VmStateError::MissingSection(TAG_DEVICES))?,
            contract_hash: contract_hash.ok_or(VmStateError::MissingSection(TAG_CONTRACT_HASH))?,
        })
    }
}

impl SnapshotRecords for Arm64VmState {
    const ARCH_TAG: u16 = ARCH_AARCH64;

    fn encode(&self) -> Result<Vec<u8>, VmStateError> {
        Arm64VmState::encode(self)
    }

    fn decode(bytes: &[u8]) -> Result<Self, VmStateError> {
        Arm64VmState::decode(bytes)
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
    use crate::VmState;

    fn sample() -> Arm64VmState {
        let mut s = Arm64VmState::default();
        s.regs.x[0] = 0x4000_0000; // x0 = the DTB GPA, per the boot protocol
        s.regs.pc = 0x0020_0000;
        s.regs.pstate = 0x3c5; // EL1h, DAIF masked
        s.sysregs.sctlr_el1 = 0x30d0_0800;
        s.sysregs.cntkctl_el1 = 0;
        s.simd_fp.q[0] = [0x5A; 16];
        s.simd_fp.q[31] = [0xA5; 16];
        s.simd_fp.fpcr = 0x0040_0000;
        s.simd_fp.fpsr = 0x0800_009f;
        s.debug.breakpoint_value[0] = 0x1111_2222_3333_4444;
        s.debug.breakpoint_control[15] = 0x55;
        s.debug.watchpoint_value[7] = 0xAAAA_BBBB_CCCC_DDDD;
        s.debug.watchpoint_control[8] = 0x66;
        s.debug.mdscr_el1 = 0x2000;
        s.debug.trap_debug_exceptions = true;
        s.vtimer.cntv_ctl_el0 = 1;
        s.vtimer.cntv_cval_el0 = 0x1234_5678;
        s.vtimer.masked = true;
        s.vtimer.offset = 0x8765_4321;
        s.interrupts.irq = true;
        s.mp_state = MpState::Runnable;
        s.vtime.snapshot_vns = 7;
        s.hypercall = vec![9, 9, 9];
        s.devices = DeviceBlob(vec![1, 2, 3, 4]);
        s.contract_hash = [0xAB; 32];
        s
    }

    /// Return the payload start and length for `tag` in a known-valid blob.
    fn section(blob: &[u8], wanted: u16) -> (usize, usize) {
        let mut pos = HEADER_LEN;
        while pos < blob.len() {
            let tag = u16::from_le_bytes(blob[pos..pos + 2].try_into().unwrap());
            let len = u32::from_le_bytes(blob[pos + 2..pos + 6].try_into().unwrap()) as usize;
            let payload = pos + 6;
            if tag == wanted {
                return (payload, len);
            }
            pos = payload + len;
        }
        panic!("section {wanted} not present in valid arm64 blob");
    }

    #[test]
    fn round_trips_byte_deterministically() {
        let s = sample();
        let a = s.encode().unwrap();
        let b = s.encode().unwrap();
        assert_eq!(a, b, "equal values must encode to identical bytes");
        assert_eq!(Arm64VmState::decode(&a).unwrap(), s);
    }

    #[test]
    fn foreign_arch_tags_are_rejected_both_ways() {
        // An x86 blob must never decode as arm64 records…
        let x86 = VmState::default();
        let x86_bytes = x86.encode().unwrap();
        assert_eq!(
            Arm64VmState::decode(&x86_bytes),
            Err(VmStateError::UnsupportedArch(crate::ARCH_X86_64))
        );
        // …and an arm64 blob must never decode as x86 records.
        let arm = sample().encode().unwrap();
        assert_eq!(
            VmState::decode(&arm),
            Err(VmStateError::UnsupportedArch(ARCH_AARCH64))
        );
    }

    #[test]
    fn strict_decode_rejects_malformed_blobs() {
        let good = sample().encode().unwrap();

        // Truncated header / body.
        assert_eq!(
            Arm64VmState::decode(&good[..4]),
            Err(VmStateError::Truncated)
        );
        assert_eq!(
            Arm64VmState::decode(&good[..good.len() - 1]),
            Err(VmStateError::Truncated)
        );

        // Trailing bytes after the final section.
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(
            Arm64VmState::decode(&trailing),
            Err(VmStateError::TrailingBytes)
        );

        // Bad magic.
        let mut bad_magic = good.clone();
        bad_magic[0] ^= 0xFF;
        assert!(matches!(
            Arm64VmState::decode(&bad_magic),
            Err(VmStateError::BadMagic(_))
        ));

        // Unsupported version.
        let mut bad_version = good.clone();
        bad_version[4] = 0xEE;
        assert!(matches!(
            Arm64VmState::decode(&bad_version),
            Err(VmStateError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn retained_state_sections_reject_noncanonical_boolean_and_reserved_bytes() {
        let good = sample().encode().unwrap();

        // Debug: four 16-entry u64 arrays + MDSCR precede the two booleans.
        let (debug, debug_len) = section(&good, TAG_DEBUG);
        assert_eq!(debug_len, size_of::<Arm64DebugWire>());
        for offset in [debug + 520, debug + 521, debug + 522] {
            let mut malformed = good.clone();
            malformed[offset] = 2;
            assert_eq!(
                Arm64VmState::decode(&malformed),
                Err(VmStateError::InvalidField)
            );
        }

        // Vtimer: three u64s, then the boolean and seven reserved bytes.
        let (vtimer, vtimer_len) = section(&good, TAG_VTIMER);
        assert_eq!(vtimer_len, size_of::<Arm64VtimerWire>());
        for offset in [vtimer + 24, vtimer + 25] {
            let mut malformed = good.clone();
            malformed[offset] = 2;
            assert_eq!(
                Arm64VmState::decode(&malformed),
                Err(VmStateError::InvalidField)
            );
        }

        // Interrupts: IRQ, FIQ, then six reserved bytes.
        let (interrupts, interrupts_len) = section(&good, TAG_INTERRUPTS);
        assert_eq!(interrupts_len, size_of::<Arm64InterruptsWire>());
        for offset in [interrupts, interrupts + 1, interrupts + 2] {
            let mut malformed = good.clone();
            malformed[offset] = 2;
            assert_eq!(
                Arm64VmState::decode(&malformed),
                Err(VmStateError::InvalidField)
            );
        }
    }

    #[test]
    fn decode_never_panics_on_arbitrary_prefixes() {
        // Totality over every truncation point of a valid blob (rule #4).
        let good = sample().encode().unwrap();
        for n in 0..good.len() {
            let _ = Arm64VmState::decode(&good[..n]);
        }
    }

    #[test]
    fn snapshot_records_surface_matches_the_inherent_codec() {
        let s = sample();
        assert_eq!(<Arm64VmState as SnapshotRecords>::ARCH_TAG, ARCH_AARCH64);
        assert_eq!(
            <Arm64VmState as SnapshotRecords>::encode(&s).unwrap(),
            s.encode().unwrap()
        );
        assert_eq!(s.vtime(), &s.vtime);
        assert_eq!(s.timers(), &s.timers);
        assert_eq!(s.entropy_bytes(), &s.hypercall[..]);
    }
}
