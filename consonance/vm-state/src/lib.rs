// SPDX-License-Identifier: AGPL-3.0-or-later
//! # vm-state — versioned `vm_state` snapshot codec
//!
//! A snapshot of a running VM has two parts: the guest *memory* (owned by
//! `snapshot-store` as copy-on-write page layers) and an opaque `vm_state` blob
//! capturing everything else that can influence future guest-visible behavior.
//! This crate is the codec for that blob: a versioned, deterministic, little-
//! endian **TLV (tag-length-value) container** that round-trips the non-memory
//! machine state — GPRs, segment/control registers, XCR0, debug registers,
//! pending events, MP state, the contract's MSR set, the XSAVE image, the V-time
//! block, the timer queue, the hypercall dispatcher's saved state, a device
//! placeholder, and the CPU/MSR contract hash — byte-identically across machines
//! and toolchains.
//!
//! The crate does **not** touch `/dev/kvm`: the vmm-core adapter reads the live
//! machine via ioctls and fills the plain-data structs here; this crate only
//! encodes them to bytes and decodes them back. Per Convention rule #2 it depends
//! on no sibling crate — V-time, timer, hypercall, and device state are mirrored
//! as local plain data or opaque byte sections, exactly as `snapshot-store`
//! treats `vm_state` as opaque bytes.
//!
//! ## Format and the version contract
//!
//! The blob is hashed into the determinism gate and must encode byte-identically
//! for identical state, so the layout is an explicitly specified little-endian
//! binary container (house style — cf. `hypercall-proto`'s frames), not a text
//! format and not a third-party crate's byte layout. The format **version is
//! part of the determinism contract**: [`VmState::decode`] rejects a version it
//! does not understand ([`VmStateError::UnsupportedVersion`]) rather than
//! silently misreading. Every v1 tag is present exactly once; a missing,
//! unknown, duplicate, or out-of-order section is a decode error, never a
//! best-effort zero-filled restore.
//!
//! ## What this crate deliberately does *not* hold
//!
//! - **No armed-but-unfired injection plan.** INTEGRATION.md §4 requires vmm-core
//!   to snapshot only at a quiescent point and to enforce that with an assertion;
//!   there is therefore no plan field to serialize.
//! - **`contract_hash` is carried, not verified.** The 32-byte hash round-trips
//!   like any field; comparing it against the current contract on restore (and
//!   rejecting a mismatch) is vmm-core's job — this crate only guarantees the
//!   value is present in every blob.
//! - **The device section is a placeholder.** [`DeviceBlob`] is opaque, length-
//!   delimited bytes until task 13's typed `LapicState` is folded in under a
//!   bumped [`VM_STATE_VERSION`]; see its docs.
//!
//! This crate writes **no hand-written `unsafe`**; the only `unsafe` is what
//! `zerocopy`'s derives generate for the fixed wire records. That still puts it
//! under the unsafe⇒Miri review rule, and Miri earns its keep here — it validates
//! the manual TLV byte-parsing and the `zerocopy` record reads on the decode path
//! (`cargo +nightly miri test -p vm-state`, run in CI).

mod codec;
mod error;
mod types;
mod wire;

pub use error::VmStateError;
pub use types::{
    DebugRegs, DeviceBlob, MpState, MsrBlock, Segment, TimerEntry, TimerQueueState, VcpuEvents,
    VcpuRegs, VcpuSregs, VtimeState, Xcrs, XsaveImage,
};

/// Container magic: `"VMS1"` read little-endian (distinct from the hypercall
/// magic `0x31504348`).
pub const VM_STATE_MAGIC: u32 = 0x3153_4D56;

/// The format version this build writes and is the only version it decodes.
pub const VM_STATE_VERSION: u16 = 1;

/// The complete non-memory machine snapshot.
///
/// The vmm-core adapter fills this from KVM ioctls plus the V-time / hypercall /
/// device subsystems; this crate encodes it ([`VmState::encode`]) and decodes it
/// back ([`VmState::decode`]). Equal `VmState`s encode to identical bytes.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct VmState {
    /// `KVM_GET_REGS` — GPRs, RIP, RFLAGS.
    pub regs: VcpuRegs,
    /// `KVM_GET_SREGS2` — segments, control registers, `IA32_APIC_BASE`, `EFER`.
    pub sregs: VcpuSregs,
    /// `KVM_GET_XCRS` — `XCR0` (the state image is in `xsave`; `XCR0` is not).
    pub xcrs: Xcrs,
    /// `KVM_GET_DEBUGREGS` — `DR0..DR3`, `DR6`, `DR7`.
    pub debugregs: DebugRegs,
    /// `KVM_GET_VCPU_EVENTS` — pending exception/NMI/SMI and interrupt shadow.
    pub events: VcpuEvents,
    /// `KVM_GET_MP_STATE` — runnable vs halted (the HLT quiescent point).
    pub mp_state: MpState,
    /// `KVM_GET_MSRS` over the contract's `allow-stateful` set.
    pub msrs: MsrBlock,
    /// `KVM_GET_XSAVE2` — the FPU/XSAVE state image.
    pub xsave: XsaveImage,
    /// V-time clock snapshot (`snapshot_vns` + ratio config), mirrored from
    /// `vtime`.
    pub vtime: VtimeState,
    /// Absolute-V-time timer-queue contents.
    pub timers: TimerQueueState,
    /// `hypercall-proto` `Dispatcher::save_state()` bytes (opaque here).
    pub hypercall: Vec<u8>,
    /// LAPIC + PIC + PIT device state — a placeholder; see [`DeviceBlob`].
    pub devices: DeviceBlob,
    /// SHA-256 of the ratified CPU/MSR contract this snapshot was taken under
    /// (CPU-MSR-CONTRACT §6). Carried so the restorer can reject a blob whose
    /// CPUID/MSR behavior has since changed; **compared by vmm-core, not here**.
    pub contract_hash: [u8; 32],
}
