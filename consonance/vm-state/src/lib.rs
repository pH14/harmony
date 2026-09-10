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
//! block, the timer queue, the hypercall dispatcher's saved state, an opaque
//! engine-owned state record, a device placeholder, and the CPU/MSR contract hash
//! — byte-identically across machines and toolchains.
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
//! part of the determinism contract**: x86 [`VmState::decode`] accepts the v3,
//! v4, and v5 record sets, and rejects every other version
//! ([`VmStateError::UnsupportedVersion`]) rather than silently misreading. The
//! v3 writer shape is retained when the engine-owned state and extended x86 CPU
//! fields are empty; a nonempty engine state selects v4 unless those CPU fields
//! require the v5 records. Every required tag is present exactly once; a
//! missing, unknown, duplicate, or out-of-order section is a decode error,
//! never a best-effort zero-filled restore.
//!
//! ## What this crate deliberately does *not* hold
//!
//! - **No armed-but-unfired injection plan.** docs/ARCHITECTURE.md requires vmm-core
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

/// Container magic: `"VMS1"` read little-endian (distinct from the hypercall
/// magic `0x31504348`).
pub const VM_STATE_MAGIC: u32 = 0x3153_4D56;

/// The newest x86 format version this build writes and decodes.
///
/// **v3** removes the retired instruction-count conversion ratio from the
/// virtual-time section; virtual time is now accumulated directly from VM exits.
/// **v4** adds a required trailing engine-owned opaque state section when that
/// state is nonempty. **v5** uses extended x86 SREGS and DEBUGREGS records when
/// their newly captured fields are nonzero; its engine-state section is
/// optional. For byte compatibility, x86 encoding retains the v3 or v4 bytes
/// whenever those new CPU fields are zero. ARM remains on its v3/v4 record set;
/// [`Arm64VmState::decode`] rejects x86-only v5 blobs.
/// **v2** (`docs/ARCHITECTURE.md`) added the container header's **arch
/// tag**: the register/sysreg record set a blob carries is per-architecture, and
/// the record *tags* alone cannot tell an x86 `REGS` section from an arm64 one —
/// two different record sets would decode into each other's fields. The tag makes
/// that a loud [`VmStateError::UnsupportedArch`] instead of a silent
/// reinterpretation. A v1 blob (no tag) is rejected at the version gate, never
/// parsed with the v2 reader.
pub const VM_STATE_VERSION: u16 = 5;

/// The v4 record version used for the engine-state extension. This remains
/// crate-private because the public latest version is the x86 v5 format, while
/// ARM continues to use this v4 shape.
pub(crate) const VM_STATE_ENGINE_VERSION: u16 = 4;

/// The legacy format version retained for byte-identical snapshots whose
/// engine-owned state and extended CPU fields are empty. It remains readable
/// but is never emitted for a
/// nonempty [`VmState::engine_state`] or [`Arm64VmState::engine_state`].
pub const VM_STATE_LEGACY_VERSION: u16 = 3;

/// The **arch tag** of the x86-64 record set ([`VmState`])
/// (`docs/ARCHITECTURE.md` — "arm64 record set; same TLV container;
/// `VM_STATE_VERSION` bump + arch tag in the header"). A vendor's records are
/// only ever decoded under its own tag; an unknown tag is
/// [`VmStateError::UnsupportedArch`], never a reinterpretation of foreign bytes.
pub const ARCH_X86_64: u16 = 1;

/// The **arch tag** reserved for the arm64 record set (the `hm-cbt` ARM
/// skeleton's `Arm64VmState`). Reserved here with the [`SnapshotRecords`] seam
/// so no other record set can ever claim the value; a blob carrying it is
/// rejected by [`VmState::decode`] as [`VmStateError::UnsupportedArch`], never
/// reinterpreted as x86 records.
pub const ARCH_AARCH64: u16 = 2;

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
    /// (x86 CPU contract). Carried so the restorer can reject a blob whose
    /// CPUID/MSR behavior has since changed; **compared by vmm-core, not here**.
    pub contract_hash: [u8; 32],
    /// Opaque state owned by the architecture-neutral engine. Empty preserves
    /// the v3 wire shape when the extended x86 CPU fields are also empty;
    /// nonempty state selects v4 when those fields are zero and v5 otherwise.
    /// The codec does not interpret these bytes.
    pub engine_state: Vec<u8>,
}
