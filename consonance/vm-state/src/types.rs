// SPDX-License-Identifier: AGPL-3.0-or-later
//! The plain-data structs that make up a [`VmState`]. These mirror the live
//! machine state the vmm-core adapter reads via KVM ioctls and the V-time /
//! timer subsystems; this crate only encodes and decodes them and depends on no
//! sibling crate (Convention rule #2).

use std::collections::BTreeMap;

/// General-purpose registers, instruction pointer, and flags — the contents of
/// `KVM_GET_REGS` (`struct kvm_regs`). Field order mirrors that ABI; the wire
/// encoding is this crate's own little-endian layout, not KVM's struct.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[allow(missing_docs)] // the register names are self-documenting
pub struct VcpuRegs {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
}

/// A segment descriptor cache entry (one of CS/DS/ES/FS/GS/SS/TR/LDT) as
/// reported by `KVM_GET_SREGS2`. `flags` packs the L/DB/G/AVL/unusable bits.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Segment {
    /// Segment base address.
    pub base: u64,
    /// Segment limit.
    pub limit: u32,
    /// Segment selector.
    pub selector: u16,
    /// Type field (the `type` of `struct kvm_segment`; `type` is a Rust keyword).
    pub type_: u8,
    /// Packed present / DPL / S (descriptor-type) bits.
    pub present_dpl_s: u8,
    /// Packed L / DB / G / AVL / unusable bits.
    pub flags: u8,
}

/// Segment and system registers, control registers, `EFER`, and the APIC base —
/// the contents of `KVM_GET_SREGS2`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[allow(missing_docs)] // the register/segment names are self-documenting
pub struct VcpuSregs {
    pub cs: Segment,
    pub ds: Segment,
    pub es: Segment,
    pub fs: Segment,
    pub gs: Segment,
    pub ss: Segment,
    pub tr: Segment,
    pub ldt: Segment,
    pub gdt_base: u64,
    pub gdt_limit: u16,
    pub idt_base: u64,
    pub idt_limit: u16,
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub cr8: u64,
    pub efer: u64,
    pub apic_base: u64,
}

/// Extended control registers — `KVM_GET_XCRS`. Only `XCR0` is captured; the
/// state image it selects lives in [`XsaveImage`], `XCR0` itself does not.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Xcrs {
    /// The live `XCR0` value (the guest may `XSETBV` within the contract's
    /// §2-masked menu).
    pub xcr0: u64,
}

/// Debug registers — `KVM_GET_DEBUGREGS`: `DR0..DR3`, `DR6`, `DR7`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct DebugRegs {
    /// `DR0`..`DR3` debug-address registers.
    pub db: [u64; 4],
    /// `DR6` debug-status register.
    pub dr6: u64,
    /// `DR7` debug-control register.
    pub dr7: u64,
}

/// Pending-event and interrupt-shadow state — `KVM_GET_VCPU_EVENTS`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VcpuEvents {
    /// Whether an exception is pending injection.
    pub exception_pending: bool,
    /// The pending exception vector (meaningful only when `exception_pending`).
    pub exception_vector: u8,
    /// The pending exception error code.
    pub exception_error_code: u32,
    /// Whether an NMI is pending.
    pub nmi_pending: bool,
    /// Whether an SMI is pending.
    pub smi_pending: bool,
    /// Interrupt-shadow blocking bits (STI / MOV-SS).
    pub interrupt_shadow: u8,
}

/// Multiprocessor run state — `KVM_GET_MP_STATE`. The codec carries only the two
/// states this single-vCPU determinism model uses; `Halted` is the HLT quiescent
/// point a snapshot is taken at.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MpState {
    /// `KVM_MP_STATE_RUNNABLE`.
    #[default]
    Runnable,
    /// `KVM_MP_STATE_HALTED`.
    Halted,
}

/// MSRs captured over the contract's `allow-stateful` set — `KVM_GET_MSRS`.
/// A `BTreeMap` keyed by MSR index so iteration order (and thus the encoded
/// bytes) is deterministic regardless of capture order (Convention rule #4);
/// never a `HashMap`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MsrBlock(pub BTreeMap<u32, u64>);

/// The FPU/XSAVE state image — `KVM_GET_XSAVE2`. Opaque, length-prefixed bytes
/// to this crate (typically up to ~4 KiB for the contract's XCR0 menu); the
/// component layout is the CPU's, not ours to interpret.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct XsaveImage(pub Vec<u8>);

/// Mirror of `vtime::VClockConfig` plus the captured `snapshot_vns`. Plain data;
/// this crate does **not** depend on `vtime`.
///
/// Snapshot-bearing configs must use an integer ratio (`ratio_den == 1`) per
/// INTEGRATION.md §4 — [`VmState::encode`](crate::VmState::encode) rejects a
/// fractional ratio with [`VmStateError::FractionalRatio`](crate::VmStateError)
/// so a non-restorable blob can never be written.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct VtimeState {
    /// Numerator of the work→nanosecond ratio.
    pub ratio_num: u64,
    /// Denominator of the work→nanosecond ratio. Must be `1` to be encodable.
    pub ratio_den: u64,
    /// Virtual TSC frequency in Hz.
    pub tsc_hz: u64,
    /// TSC value corresponding to `vns == 0`.
    pub tsc_base: u64,
    /// The captured `VClock::snapshot_vns(work)` result (whole nanoseconds).
    pub snapshot_vns: u64,
}

/// One scheduled timer: an absolute V-time deadline (survives restore unchanged),
/// tagged with its task-05 insertion sequence `seq` so same-deadline firing order
/// is reproducible.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TimerEntry {
    /// Absolute V-time deadline in nanoseconds.
    pub deadline_vns: u64,
    /// The queue's monotonic insertion sequence number for this entry; the
    /// FIFO tie-breaker among equal deadlines.
    pub seq: u64,
    /// The caller-chosen timer token.
    pub token: u64,
    /// Re-arm period in V-ns; `0` for a one-shot timer.
    pub period_vns: u64,
}

/// Timer-queue contents.
///
/// `entries` are kept sorted by `(deadline_vns, seq)` — the queue's firing
/// order, **not** token order: task-05's `TimerQueue` fires same-deadline timers
/// in FIFO insertion order, so the snapshot carries `seq` to reproduce it.
/// `next_seq` is the queue's monotonic counter, snapshotted so a restored queue
/// keeps issuing non-colliding sequence numbers.
///
/// A faithful queue obeys three task-05 invariants, all enforced as **value
/// invariants** by [`VmState::encode`](crate::VmState::encode) (and re-checked by
/// [`VmState::decode`](crate::VmState::decode)) — a violation is
/// [`VmStateError::InvalidField`](crate::VmStateError), never a silent fix-up, so
/// `decode(encode(s)?) == s` holds for every accepted `VmState`:
///
/// 1. entries strictly ascending and unique by `(deadline_vns, seq)`;
/// 2. `token`s unique across entries (the queue keys a `token -> entry` index, so
///    a duplicate would misdirect a later cancel/reschedule);
/// 3. every `seq < next_seq` (else a restored queue's next same-deadline insert
///    would reuse a live `seq`).
///
/// Timer state is genuinely insertion-order-dependent: unlike the MSR
/// `BTreeMap`, two queues built by different insertion orders are *different*
/// states and (once canonicalized to firing order) encode to whatever that order
/// dictates — same firing order ⇒ identical bytes.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TimerQueueState {
    /// Pending timers, canonically ordered by `(deadline_vns, seq)`.
    pub entries: Vec<TimerEntry>,
    /// The queue's next insertion sequence number.
    pub next_seq: u64,
}

/// **Placeholder** for the device-emulation state (LAPIC + PIC stub + PIT stub).
///
/// Carried now as opaque, length-prefixed bytes so the container format and
/// version lock without waiting on task 13's `lapic::LapicState`. The vmm-core
/// adapter passes through whatever the device models emit; this crate does not
/// interpret it. The format stays forward-compatible because this is one tag
/// whose internal layout can gain a typed encoding under a bumped
/// [`VM_STATE_VERSION`](crate::VM_STATE_VERSION) without disturbing any other
/// section.
// TODO(task-13): replace with a typed { lapic, pic, pit } record.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct DeviceBlob(pub Vec<u8>);
