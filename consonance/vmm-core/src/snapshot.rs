// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live VM snapshot / branch: the wiring that joins `snapshot-store` (the layered
//! copy-on-write guest-memory page store) and `vm-state` (the versioned codec for
//! the non-memory machine blob) to the live VM in [`crate::vmm`].
//!
//! This module is "the elsewhere" both sibling crates defer their KVM side to
//! (their docs say the KVM integration "lives elsewhere"). It holds two things:
//!
//! - **[`SnapshotEngine`]** — a thin owner of a [`snapshot_store::Store`] that turns
//!   a full guest-memory image + a sealed `vm_state` blob into a content-addressed
//!   snapshot (`begin_base` → `write_page` per frame → `seal`), derives later
//!   snapshots from the pages dirtied since a parent (`derive`), and materializes a
//!   snapshot back into a private CoW [`Mapping`]. Capture is **dirty-set-
//!   proportional**: the store discards a written page whose content already
//!   resolves through the parent chain, so a derived snapshot's `owned_pages` counts
//!   only genuinely-changed frames, and identical page contents are stored **once
//!   store-wide** — so N VMs forked from one boot share a single resident base.
//!
//! - **The `vm_state` adapter** (pure, bidirectional conversions, exercised here and
//!   driven from [`crate::vmm::Vmm`]) — converts between the live machine's
//!   [`vmm_backend::VcpuState`] / V-time / userspace-xAPIC / legacy-platform state
//!   and `vm-state`'s plain-data records, plus a vmm-core-owned **device blob** (the
//!   `vm_state::DeviceBlob` placeholder) that carries the xAPIC + 8259 IMR + PCI
//!   latch + 8250 UART + `IA32_TSC_ADJUST` state the typed records have no field for.
//!
//! The KVM-specific mechanics this builds on — the dirty-log harvest that yields the
//! per-snapshot dirty set, and the memslot remap that makes restore O(dirty) rather
//! than O(image) — live **below the `Backend` trait** in `vmm-backend` (task 08's
//! measured mechanism); see `IMPLEMENTATION.md`. The engine here is portable and
//! Mac/Miri-testable against plain memory, exactly as `snapshot-store` is.

use lapic::LapicState;
use snapshot_store::{Mapping, PAGE_SIZE, SnapStats, SnapshotId, Store, StoreConfig, StoreStats};
use vm_state::{
    DebugRegs, DeviceBlob, MpState, MsrBlock, Segment, TimerQueueState, VcpuEvents, VcpuRegs,
    VcpuSregs, VmState, Xcrs, XsaveImage,
};

/// Errors from the snapshot/branch path: a store failure, a `vm_state` codec
/// failure, a malformed vmm-core device blob, a guest-image size mismatch, a
/// LAPIC restore rejection, or a snapshot taken under a different CPU/MSR contract.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// An underlying [`snapshot_store::Store`] operation failed.
    #[error("snapshot-store error")]
    Store(#[from] snapshot_store::StoreError),
    /// The `vm_state` blob failed to encode or decode (strict, total codec).
    #[error("vm_state codec error")]
    Codec(#[from] vm_state::VmStateError),
    /// A guest-memory image's length is not the configured image size.
    #[error("guest image is {got} bytes, expected {expected} ({pages} pages × {PAGE_SIZE})")]
    MemorySize {
        /// The offending image length in bytes.
        got: usize,
        /// The configured image length in bytes (`pages * PAGE_SIZE`).
        expected: usize,
        /// The configured image size in pages.
        pages: u64,
    },
    /// The vmm-core-owned device blob (inside `vm_state::DeviceBlob`) was malformed
    /// — truncated, a bad magic/version, or an out-of-range field. Total, never a
    /// panic (Convention rule #4).
    #[error("device blob malformed: {0}")]
    DeviceBlob(&'static str),
    /// A harvested dirty-page gfn lies outside the configured guest image.
    #[error("dirty gfn {gfn} out of range: guest image is {pages} pages")]
    DirtyGfnOutOfRange {
        /// The offending guest frame number.
        gfn: u64,
        /// The configured guest image size in pages.
        pages: u64,
    },
    /// The userspace xAPIC rejected a restored [`LapicState`].
    #[error("lapic restore rejected: {0}")]
    Lapic(&'static str),
    /// The snapshot was taken under a different ratified CPU/MSR contract than the
    /// one this VMM enforces, so its CPUID/MSR behavior would silently diverge on
    /// restore. Refused loudly (INTEGRATION.md §4 `contract_hash`).
    #[error("contract hash mismatch: snapshot taken under a different CPU/MSR contract")]
    ContractMismatch,
}

/// The live-VM snapshot / branch engine: a [`snapshot_store::Store`] sized to the
/// guest image, plus the page count.
///
/// One engine backs a whole exploration tree: a single base layer holds the booted
/// image, every later snapshot records only its dirtied pages, and identical page
/// contents are interned once store-wide so N branches from one boot do not cost N
/// copies. `vm_state` blobs are sealed verbatim (the canonical `vm_state::VmState`
/// encoding), opaque to the store.
pub struct SnapshotEngine {
    store: Store,
    mem_pages: u64,
}

impl SnapshotEngine {
    /// Create an engine for guest images of `mem_bytes` bytes. `mem_bytes` must be a
    /// non-zero multiple of [`PAGE_SIZE`]; otherwise the engine still works but the
    /// final partial page is simply never addressable.
    pub fn new(mem_bytes: usize) -> SnapshotEngine {
        let mem_pages = (mem_bytes / PAGE_SIZE) as u64;
        SnapshotEngine {
            store: Store::new(StoreConfig { mem_pages }),
            mem_pages,
        }
    }

    /// The configured guest image size in pages.
    pub fn mem_pages(&self) -> u64 {
        self.mem_pages
    }

    /// Read-only access to the underlying store (for `store_stats` / `stats`).
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Store-wide statistics — the **N-VMs-share-one-base** evidence: a base plus N
    /// derived snapshots that touched nothing keep `stored_unique_pages` at the base's
    /// distinct-content count, never `N ×` it (gate 3).
    pub fn store_stats(&self) -> StoreStats {
        self.store.store_stats()
    }

    /// Per-snapshot statistics (`owned_pages` = pages this layer provides that no
    /// ancestor provides identically — the dirty set actually retained).
    pub fn stats(&self, snap: SnapshotId) -> Result<SnapStats, SnapshotError> {
        Ok(self.store.stats(snap)?)
    }

    /// Build the **base** layer from a full guest-memory image and a sealed blob:
    /// `begin_base` → `write_page` per guest frame → `seal(vm_state)`. Pages whose
    /// content is the all-zero page cost nothing (sparse images are free).
    ///
    /// `vm_state` is the canonical [`vm_state::VmState::encode`] bytes (opaque to the
    /// store). `memory` must be exactly `mem_pages * PAGE_SIZE` bytes.
    pub fn snapshot_base(
        &mut self,
        memory: &[u8],
        vm_state: &[u8],
    ) -> Result<SnapshotId, SnapshotError> {
        self.check_image_len(memory)?;
        let mut builder = self.store.begin_base();
        for (gfn, frame) in memory.chunks_exact(PAGE_SIZE).enumerate() {
            builder.write_page(gfn as u64, frame)?;
        }
        Ok(builder.seal(vm_state.to_vec()))
    }

    /// Derive a child snapshot of `parent` from the current full image.
    ///
    /// When `dirty` is `Some(gfns)`, only those frames are written — the **dirty-set-
    /// proportional** path the KVM dirty-log harvest feeds (each later snapshot pays
    /// only for what changed). When `dirty` is `None`, every frame is written and the
    /// store's seal-time dedup keeps the result equally cheap (a frame whose content
    /// already resolves through the parent chain is discarded), so capture is correct
    /// even without a harvested dirty set — only the capture *cost* differs.
    pub fn snapshot_derive(
        &mut self,
        parent: SnapshotId,
        memory: &[u8],
        dirty: Option<&[u64]>,
        vm_state: &[u8],
    ) -> Result<SnapshotId, SnapshotError> {
        self.check_image_len(memory)?;
        let mut builder = self.store.derive(parent)?;
        match dirty {
            Some(gfns) => {
                for &gfn in gfns {
                    if gfn >= self.mem_pages {
                        return Err(SnapshotError::DirtyGfnOutOfRange {
                            gfn,
                            pages: self.mem_pages,
                        });
                    }
                    // gfn < mem_pages and the image length was checked == mem_pages *
                    // PAGE_SIZE, so this frame is always fully in range (no panic).
                    let off = gfn as usize * PAGE_SIZE;
                    builder.write_page(gfn, &memory[off..off + PAGE_SIZE])?;
                }
            }
            None => {
                for (gfn, frame) in memory.chunks_exact(PAGE_SIZE).enumerate() {
                    builder.write_page(gfn as u64, frame)?;
                }
            }
        }
        Ok(builder.seal(vm_state.to_vec()))
    }

    /// Materialize `snap`'s full logical image as a private copy-on-write
    /// [`Mapping`] — the host backing the restore points the KVM memslot at (the
    /// remap mechanism task 08 chose; below the trait). Resolving the chain is
    /// O(chain) per gfn, memoized; only non-zero pages touch the sparse tempfile.
    pub fn materialize(&self, snap: SnapshotId) -> Result<Mapping, SnapshotError> {
        Ok(self.store.materialize(snap)?)
    }

    /// Decode the sealed `vm_state` blob of `snap` back into a [`VmState`].
    pub fn vm_state(&self, snap: SnapshotId) -> Result<VmState, SnapshotError> {
        Ok(VmState::decode(self.store.vm_state(snap)?)?)
    }

    /// Increment `snap`'s refcount (an explorer holding a fork alive). See
    /// [`snapshot_store::Store::retain`].
    pub fn retain(&mut self, snap: SnapshotId) -> Result<(), SnapshotError> {
        Ok(self.store.retain(snap)?)
    }

    /// Decrement `snap`'s refcount. See [`snapshot_store::Store::release`].
    pub fn release(&mut self, snap: SnapshotId) -> Result<(), SnapshotError> {
        Ok(self.store.release(snap)?)
    }

    /// Reap layers unreachable from any live snapshot; returns bytes freed.
    pub fn gc(&mut self) -> u64 {
        self.store.gc()
    }

    fn check_image_len(&self, memory: &[u8]) -> Result<(), SnapshotError> {
        let expected = (self.mem_pages as usize).saturating_mul(PAGE_SIZE);
        if memory.len() != expected {
            return Err(SnapshotError::MemorySize {
                got: memory.len(),
                expected,
                pages: self.mem_pages,
            });
        }
        Ok(())
    }
}

// ===========================================================================
// vm_state adapter — pure, bidirectional conversions between the live machine's
// `vmm_backend` value types and `vm-state`'s plain-data records.
//
// The live `VcpuState` carries a superset of `vm-state`'s `VcpuEvents` /
// `VcpuSregs` (KVM exposes more pending-event and SREGS2 detail than the
// determinism model's typed records carry). The typed `vm-state::VcpuEvents` is a
// reduced 6-field subset; task 41 captures the **full** `kvm_vcpu_events` in the
// vmm-core-owned device blob (see `DeviceState::events`) and makes it authoritative
// on restore, so an **in-flight interrupt/exception injection** — a non-quiescent
// point — now round-trips bit-identically rather than being fail-closed-rejected.
// The still-dropped `kvm_sregs2` `flags`/`pdptrs` (PAE-only; the long-mode /
// paging-off determinism guest never uses the PAE PDPTRs) and `debugregs.flags` are
// zero at any V-time point, so refusing a (non-existent) non-zero value there only
// guards misuse. This is documented in IMPLEMENTATION.md.
// ===========================================================================

/// `VcpuState.regs` → `vm_state::VcpuRegs` (identical field set, flat copy).
pub(crate) fn to_vm_regs(r: &vmm_backend::VcpuRegs) -> VcpuRegs {
    VcpuRegs {
        rax: r.rax,
        rbx: r.rbx,
        rcx: r.rcx,
        rdx: r.rdx,
        rsi: r.rsi,
        rdi: r.rdi,
        rsp: r.rsp,
        rbp: r.rbp,
        r8: r.r8,
        r9: r.r9,
        r10: r.r10,
        r11: r.r11,
        r12: r.r12,
        r13: r.r13,
        r14: r.r14,
        r15: r.r15,
        rip: r.rip,
        rflags: r.rflags,
    }
}

/// `vm_state::VcpuRegs` → `VcpuState.regs` (reverse of [`to_vm_regs`]).
pub(crate) fn from_vm_regs(r: &VcpuRegs) -> vmm_backend::VcpuRegs {
    vmm_backend::VcpuRegs {
        rax: r.rax,
        rbx: r.rbx,
        rcx: r.rcx,
        rdx: r.rdx,
        rsi: r.rsi,
        rdi: r.rdi,
        rsp: r.rsp,
        rbp: r.rbp,
        r8: r.r8,
        r9: r.r9,
        r10: r.r10,
        r11: r.r11,
        r12: r.r12,
        r13: r.r13,
        r14: r.r14,
        r15: r.r15,
        rip: r.rip,
        rflags: r.rflags,
    }
}

/// Pack a live segment's separate present/dpl/s/db/l/g/avl/unusable bytes into the
/// two `vm_state::Segment` bytes (`present_dpl_s`, `flags`). Reversible by
/// [`unpack_segment`]; every source field is a 0/1 (dpl 0..=3) bit, so the packing
/// is lossless.
fn pack_segment(s: &vmm_backend::Segment) -> Segment {
    let present_dpl_s = (s.present & 1) | ((s.dpl & 3) << 1) | ((s.s & 1) << 3);
    let flags = (s.db & 1)
        | ((s.l & 1) << 1)
        | ((s.g & 1) << 2)
        | ((s.avl & 1) << 3)
        | ((s.unusable & 1) << 4);
    // Canonicalize an **unusable** segment's `type` to 0 — mirror `vmm::encode_segment` (the
    // VCPU hash chunk). An unusable segment's hidden type is architecturally don't-care (SDM
    // Vol. 3 §24.4.1), and KVM normalizes it `0 → 1` across `KVM_SET_SREGS` → `KVM_GET_SREGS`.
    // This typed `vm_state::Segment` rides the **VMST** hash chunk (`wire_snapshot_hashing`),
    // so without masking here a `save → restore → save` would perturb this don't-care field
    // and the VMST chunk's `state_hash` would diverge from the source even though the VCPU
    // chunk is canonical (PR #12 round 4). Golden-safe: a live `KVM_GET_SREGS` already reports
    // `type = 0` for an unusable segment, so masking is a no-op for every real capture.
    let type_ = if s.unusable != 0 { 0 } else { s.type_ };
    Segment {
        base: s.base,
        limit: s.limit,
        selector: s.selector,
        type_,
        present_dpl_s,
        flags,
    }
}

/// Reverse of [`pack_segment`].
fn unpack_segment(s: &Segment) -> vmm_backend::Segment {
    vmm_backend::Segment {
        base: s.base,
        limit: s.limit,
        selector: s.selector,
        type_: s.type_,
        present: s.present_dpl_s & 1,
        dpl: (s.present_dpl_s >> 1) & 3,
        s: (s.present_dpl_s >> 3) & 1,
        db: s.flags & 1,
        l: (s.flags >> 1) & 1,
        g: (s.flags >> 2) & 1,
        avl: (s.flags >> 3) & 1,
        unusable: (s.flags >> 4) & 1,
    }
}

/// `VcpuState.sregs` → `vm_state::VcpuSregs` (segments packed, descriptor tables
/// flattened to base/limit pairs; `kvm_sregs2` `flags`/`pdptrs` are not carried —
/// zero at the quiescent snapshot point, see the module note).
pub(crate) fn to_vm_sregs(s: &vmm_backend::VcpuSregs) -> VcpuSregs {
    VcpuSregs {
        cs: pack_segment(&s.cs),
        ds: pack_segment(&s.ds),
        es: pack_segment(&s.es),
        fs: pack_segment(&s.fs),
        gs: pack_segment(&s.gs),
        ss: pack_segment(&s.ss),
        tr: pack_segment(&s.tr),
        ldt: pack_segment(&s.ldt),
        gdt_base: s.gdt.base,
        gdt_limit: s.gdt.limit,
        idt_base: s.idt.base,
        idt_limit: s.idt.limit,
        cr0: s.cr0,
        cr2: s.cr2,
        cr3: s.cr3,
        cr4: s.cr4,
        cr8: s.cr8,
        efer: s.efer,
        apic_base: s.apic_base,
    }
}

/// `vm_state::VcpuSregs` → `VcpuState.sregs` (reverse of [`to_vm_sregs`];
/// `flags`/`pdptrs` restore to zero — valid for the long-mode / paging-off guests
/// the determinism model snapshots).
pub(crate) fn from_vm_sregs(s: &VcpuSregs) -> vmm_backend::VcpuSregs {
    vmm_backend::VcpuSregs {
        cs: unpack_segment(&s.cs),
        ds: unpack_segment(&s.ds),
        es: unpack_segment(&s.es),
        fs: unpack_segment(&s.fs),
        gs: unpack_segment(&s.gs),
        ss: unpack_segment(&s.ss),
        tr: unpack_segment(&s.tr),
        ldt: unpack_segment(&s.ldt),
        gdt: vmm_backend::DescriptorTable {
            base: s.gdt_base,
            limit: s.gdt_limit,
        },
        idt: vmm_backend::DescriptorTable {
            base: s.idt_base,
            limit: s.idt_limit,
        },
        cr0: s.cr0,
        cr2: s.cr2,
        cr3: s.cr3,
        cr4: s.cr4,
        cr8: s.cr8,
        efer: s.efer,
        apic_base: s.apic_base,
        flags: 0,
        pdptrs: [0; 4],
    }
}

/// `VcpuState.debugregs` → `vm_state::DebugRegs` (the always-zero KVM `flags` is
/// dropped).
pub(crate) fn to_vm_debugregs(d: &vmm_backend::DebugRegs) -> DebugRegs {
    DebugRegs {
        db: d.db,
        dr6: d.dr6,
        dr7: d.dr7,
    }
}

/// `vm_state::DebugRegs` → `VcpuState.debugregs`.
pub(crate) fn from_vm_debugregs(d: &DebugRegs) -> vmm_backend::DebugRegs {
    vmm_backend::DebugRegs {
        db: d.db,
        dr6: d.dr6,
        dr7: d.dr7,
        flags: 0,
    }
}

/// `VcpuState.events` → `vm_state::VcpuEvents` (the reduced 6-field typed subset:
/// pending exception vector/code, NMI/SMI pending, interrupt shadow). The **full**
/// `kvm_vcpu_events` rides the device blob (task 41) and is authoritative on the full
/// restore path; this typed record is kept for task-39 `vm-state` codec compatibility.
pub(crate) fn to_vm_events(e: &vmm_backend::VcpuEvents) -> VcpuEvents {
    VcpuEvents {
        exception_pending: e.exception_pending != 0,
        exception_vector: e.exception_nr,
        exception_error_code: e.exception_error_code,
        nmi_pending: e.nmi_pending != 0,
        smi_pending: e.smi_pending != 0,
        interrupt_shadow: e.interrupt_shadow,
    }
}

/// `vm_state::VcpuEvents` → `VcpuState.events` (only the reduced subset; the full
/// restore overwrites `events` from the device blob's complete `kvm_vcpu_events`, so
/// the injection bookkeeping this reduced record cannot express is **not** lost —
/// see [`crate::vmm::Vmm::restore_vm_state`]). Used directly only where no device
/// blob is present.
pub(crate) fn from_vm_events(e: &VcpuEvents) -> vmm_backend::VcpuEvents {
    vmm_backend::VcpuEvents {
        exception_pending: u8::from(e.exception_pending),
        exception_nr: e.exception_vector,
        exception_error_code: e.exception_error_code,
        nmi_pending: u8::from(e.nmi_pending),
        smi_pending: u8::from(e.smi_pending),
        interrupt_shadow: e.interrupt_shadow,
        ..Default::default()
    }
}

/// `VcpuState.mp_state` → `vm_state::MpState`.
pub(crate) fn to_vm_mp_state(m: vmm_backend::MpState) -> MpState {
    match m {
        vmm_backend::MpState::Runnable => MpState::Runnable,
        vmm_backend::MpState::Halted => MpState::Halted,
    }
}

/// `vm_state::MpState` → `VcpuState.mp_state`.
pub(crate) fn from_vm_mp_state(m: MpState) -> vmm_backend::MpState {
    match m {
        MpState::Runnable => vmm_backend::MpState::Runnable,
        MpState::Halted => vmm_backend::MpState::Halted,
    }
}

/// Assemble a complete [`VcpuState`](vmm_backend::VcpuState) from the typed
/// `vm_state` records (the memory-less half of a restore).
pub(crate) fn vcpu_state_from(s: &VmState) -> vmm_backend::VcpuState {
    vmm_backend::VcpuState {
        regs: from_vm_regs(&s.regs),
        sregs: from_vm_sregs(&s.sregs),
        xcr0: s.xcrs.xcr0,
        debugregs: from_vm_debugregs(&s.debugregs),
        events: from_vm_events(&s.events),
        mp_state: from_vm_mp_state(s.mp_state),
        msrs: s.msrs.0.clone(),
        xsave: s.xsave.0.clone(),
    }
}

/// Fill the typed `vm_state` records from a live [`VcpuState`](vmm_backend::VcpuState)
/// into the given [`VmState`] (memory-less half of a save). Leaves `vtime`,
/// `timers`, `hypercall`, `devices`, `contract_hash` for the caller.
pub(crate) fn fill_vcpu_state(out: &mut VmState, s: &vmm_backend::VcpuState) {
    out.regs = to_vm_regs(&s.regs);
    out.sregs = to_vm_sregs(&s.sregs);
    out.xcrs = Xcrs { xcr0: s.xcr0 };
    out.debugregs = to_vm_debugregs(&s.debugregs);
    // Project the **canonical** events into the reduced typed record — mirroring the
    // device blob (`DeviceState.events = canonical_events(...)`). A raw residual (a stale
    // `exception.nr`/`error_code` with neither `injected` nor `pending`) must NOT survive
    // here either: with `wire_snapshot_hashing()` ON, the typed record rides the `VMST`
    // hash chunk, so a raw residual would make a save→restore→save round-trip's
    // `state_hash` differ from the source (the restore re-establishes the *canonical*
    // events). Canonicalizing both records keeps the full hash restore-transparent.
    out.events = to_vm_events(&canonical_events(&s.events));
    out.mp_state = to_vm_mp_state(s.mp_state);
    out.msrs = MsrBlock(s.msrs.clone());
    out.xsave = XsaveImage(s.xsave.clone());
    // vmm-core holds no `vtime::TimerQueue`: the only timer is the userspace xAPIC
    // timer, whose state rides in the device blob. So the typed timer queue is
    // empty (trivially satisfies the codec's ordering invariants).
    out.timers = TimerQueueState::default();
}

// `kvm_vcpu_events.flags` validity-mask bits (KVM uapi, stable ABI). On a
// `KVM_GET_VCPU_EVENTS` KVM reports several of these set unconditionally (they mark a
// sub-record as *present in the GET*, not *active*); on a `KVM_SET_VCPU_EVENTS` the bit
// instead means "apply this sub-record." vmm-core carries the `flags` field through the
// device blob, so it owns rebuilding it for the SET — see [`canonical_events`]. (The
// backend's `to_kvm_events` passes `flags` through verbatim; vmm-core must hand it the
// SET-meaning mask, not the GET-meaning one.)
const KVM_VCPUEVENT_VALID_NMI_PENDING: u32 = 0x0000_0001;
const KVM_VCPUEVENT_VALID_SIPI_VECTOR: u32 = 0x0000_0002;
const KVM_VCPUEVENT_VALID_SHADOW: u32 = 0x0000_0004;
const KVM_VCPUEVENT_VALID_SMM: u32 = 0x0000_0008;
const KVM_VCPUEVENT_VALID_PAYLOAD: u32 = 0x0000_0010;
const KVM_VCPUEVENT_VALID_TRIPLE_FAULT: u32 = 0x0000_0020;

/// Reduce a live `kvm_vcpu_events` to its **canonical, restorable** form (task 41).
///
/// KVM leaves *stale modifier residuals* in `kvm_vcpu_events` even at a fully quiescent
/// point: `interrupt.nr` keeps the **last-delivered** vector after delivery completes,
/// `exception.nr`/`has_error_code`/`error_code` persist from a serviced fault, and a
/// `GET` reports `flags` with `VALID_NMI_PENDING | VALID_SHADOW | VALID_SMM` set
/// *unconditionally* (they mark presence in the GET, not active state). These are **not
/// in-flight state** — they are inert (no `injected`/`pending` bit is set). Replaying
/// them verbatim into `KVM_SET_VCPU_EVENTS` on restore corrupts the resumed guest (the
/// box symptom was an immediate kernel `Oops` / `Attempted to kill the idle task`). The
/// reduced subset task 39 carried happened to drop the worst of them; the full-events
/// capture re-introduced them, so we must canonicalize.
///
/// Canonical form: each modifier is kept **only when its active bit is set**, and
/// `flags` is **rebuilt from the surviving fields** to the SET-meaning mask the backend
/// will replay (a bit set iff its sub-record is genuinely active). Restoring into a
/// fresh vCPU (all-default) then re-establishes exactly the active state — a *true*
/// in-flight injection (interrupt/exception/NMI/SMI/triple-fault, with its payload)
/// round-trips faithfully, while an inert residual collapses to the clean quiescent
/// record. Idempotent (`canonical_events(canonical_events(e)) == canonical_events(e)`).
/// The maskable-interrupt fields are carried for fidelity, but vmm-core's LAPIC seam
/// re-derives the actual injection on the first post-restore service either way.
pub(crate) fn canonical_events(e: &vmm_backend::VcpuEvents) -> vmm_backend::VcpuEvents {
    let mut c = vmm_backend::VcpuEvents::default();
    // Maskable interrupt: keep nr/soft only while an injection is genuinely in flight.
    if e.interrupt_injected != 0 {
        c.interrupt_injected = e.interrupt_injected;
        c.interrupt_nr = e.interrupt_nr;
        c.interrupt_soft = e.interrupt_soft;
    }
    // Exception: keep the sub-record only while one is injected or pending; and within it,
    // gate each VALUE field on its own validity bit (mirror the SIPI gating) so a stale
    // `error_code` / `payload` whose `has_*` bit is clear never reaches the canonical blob or
    // the `state_hash` — KVM would not apply it, and replaying untrusted residual bytes would
    // diverge a save → restore → save (PR #12 round 8 audit).
    if e.exception_injected != 0 || e.exception_pending != 0 {
        c.exception_injected = e.exception_injected;
        c.exception_pending = e.exception_pending;
        c.exception_nr = e.exception_nr;
        c.exception_has_error_code = e.exception_has_error_code;
        c.exception_error_code = if e.exception_has_error_code != 0 {
            e.exception_error_code
        } else {
            0
        };
        c.exception_has_payload = e.exception_has_payload;
        c.exception_payload = if e.exception_has_payload != 0 {
            e.exception_payload
        } else {
            0
        };
    }
    // NMI / interrupt-shadow / SMM / SIPI / triple-fault are genuine state, carried
    // verbatim (each defaults to 0 at a quiescent point). `nmi_masked` rides the NMI
    // sub-record's validity bit.
    c.nmi_injected = e.nmi_injected;
    c.nmi_pending = e.nmi_pending;
    c.nmi_masked = e.nmi_masked;
    c.interrupt_shadow = e.interrupt_shadow;
    // SIPI: gate strictly on the *original* validity bit, never on `sipi_vector != 0`.
    // Vector 0 is a legal SIPI (a value test would drop a genuine one), and a nonzero
    // vector with `VALID_SIPI_VECTOR` clear is a stale residual (a value test would
    // replay it). KVM zeroes the vector and clears the bit on every `KVM_GET_VCPU_EVENTS`
    // (it is SET-only — for injecting into a wait-for-SIPI vCPU), so a captured snapshot
    // carries no SIPI; gating on the bit keeps a synthetic/relayed record faithful.
    let sipi_valid = e.flags & KVM_VCPUEVENT_VALID_SIPI_VECTOR != 0;
    if sipi_valid {
        c.sipi_vector = e.sipi_vector;
    }
    c.smi_smm = e.smi_smm;
    c.smi_pending = e.smi_pending;
    c.smi_inside_nmi = e.smi_inside_nmi;
    c.smi_latched_init = e.smi_latched_init;
    c.triple_fault_pending = e.triple_fault_pending;
    // Rebuild the SET-meaning validity mask: set a bit iff its sub-record is active, so
    // the GET-side metadata bits (set unconditionally, with all-zero fields) are never
    // replayed. This is the form used for the **`state_hash`** (active-only, so two
    // same-seed runs and a quiescent record both hash to `flags = 0` — no golden moves).
    // NOTE: the **restore** path does NOT use these flags directly — see
    // [`events_for_restore`]. KVM treats a *clear* validity bit on `KVM_SET_VCPU_EVENTS` as
    // "leave that sub-record UNCHANGED", not "clear it", so restoring this active-only mask
    // onto a non-fresh vCPU would retain the prior occupant's stale state. `events_for_restore`
    // forces the gated clear-on-restore bits on; this function stays active-only for the hash.
    let smm_active =
        c.smi_smm != 0 || c.smi_pending != 0 || c.smi_inside_nmi != 0 || c.smi_latched_init != 0;
    c.flags = if c.nmi_injected != 0 || c.nmi_pending != 0 || c.nmi_masked != 0 {
        KVM_VCPUEVENT_VALID_NMI_PENDING
    } else {
        0
    } | if sipi_valid {
        KVM_VCPUEVENT_VALID_SIPI_VECTOR
    } else {
        0
    } | if c.interrupt_shadow != 0 {
        KVM_VCPUEVENT_VALID_SHADOW
    } else {
        0
    } | if smm_active {
        KVM_VCPUEVENT_VALID_SMM
    } else {
        0
    } | if c.exception_has_payload != 0 {
        KVM_VCPUEVENT_VALID_PAYLOAD
    } else {
        0
    } | if c.triple_fault_pending != 0 {
        KVM_VCPUEVENT_VALID_TRIPLE_FAULT
    } else {
        0
    };
    c
}

/// The canonical events to hand to `KVM_SET_VCPU_EVENTS` on **restore** — like
/// [`canonical_events`], but with the **cap-free** clear-on-restore validity bits forced **on**.
///
/// KVM treats a *clear* validity bit on `KVM_SET_VCPU_EVENTS` as **"leave that sub-record
/// UNCHANGED"**, not "clear it". So restoring a quiescent snapshot (no NMI-pending /
/// interrupt-shadow / SMM) with those bits clear onto a **non-fresh** vCPU — a committed /
/// previously-run vCPU, i.e. the branch or restore-in-place case — would **retain the previous
/// occupant's stale event state**: the restored VM would depend on its predecessor, a
/// determinism leak (PR #12 round 6, codex/GPT-5.5). Forcing the bits on, with the canonical
/// payloads (which are 0 when inactive), makes restore explicitly **clear** that state, so the
/// restored vCPU is independent of its predecessor (restore is idempotent w.r.t. target state).
///
/// **Which bits can be forced is constrained by KVM's SET-side capability gating.** A validity
/// bit whose capability is not enabled is rejected with `-EINVAL` *even with a zero payload*. We
/// force exactly the bits this backend's KVM accepts unconditionally:
/// - `NMI_PENDING`, `SHADOW` — core ABI, always valid.
/// - `SMM` — supported by default (the box's `KVM_GET_VCPU_EVENTS` reports `flags` with
///   `VALID_SMM` set: `0x0D = NMI_PENDING|SHADOW|SMM`).
/// - `TRIPLE_FAULT` — **NOT forced**: it requires `KVM_CAP_X86_TRIPLE_FAULT_EVENT`, which this
///   backend does not enable, so setting the bit is `-EINVAL` (the round-6 box run proved this).
///   With the cap off there is **no** triple-fault sub-record to leak, so leaving it gated on
///   active (via [`canonical_events`]) is both safe and complete here.
/// - `PAYLOAD` — stays gated on `exception_has_payload` (its cap is likewise not enabled); the
///   exception sub-record (injected/nr/error_code) is applied by KVM unconditionally anyway.
/// - `SIPI_VECTOR` — stays gated (SET-only; round-2 handling).
///
/// The **`state_hash`** uses [`canonical_events`] (active-only flags), not this — so forcing the
/// bits here does **not** move any golden (the hashed form is unchanged).
pub(crate) fn events_for_restore(e: &vmm_backend::VcpuEvents) -> vmm_backend::VcpuEvents {
    let mut c = canonical_events(e);
    c.flags |=
        KVM_VCPUEVENT_VALID_NMI_PENDING | KVM_VCPUEVENT_VALID_SHADOW | KVM_VCPUEVENT_VALID_SMM;
    c
}

/// Return `Some(reason)` if `vcpu` carries machine state the snapshot would
/// **silently zero** on restore — so [`crate::vmm::Vmm::save_vm_state`] can **fail
/// closed** instead of sealing a lossy blob (rather than the restore side silently
/// dropping it).
///
/// **This is the class-closing audit of `VcpuState`.** Every field is either captured
/// by the typed records / device blob, or asserted zero here, so a saved blob is
/// **provably lossless-or-rejected**:
/// - *Captured:* `regs` (all), `sregs` segments + descriptor tables + CRs + EFER +
///   APIC_BASE, `xcr0`, `debugregs.db`/`dr6`/`dr7`, **the full `events` record**
///   (every `kvm_vcpu_events` field — task 41, captured verbatim in the device blob,
///   no longer a reduced subset), `mp_state`, `msrs`, `xsave`.
/// - *Asserted zero here (not carried):* `sregs.flags`/`sregs.pdptrs` (PAE-only;
///   64-bit guest), `debugregs.flags` (KVM "currently always 0").
///
/// **Events are no longer rejected wholesale.** Task 41 captures the *entire* `kvm_vcpu_events`
/// (in-flight interrupt/exception injection, SMM, etc.) in the device blob and re-establishes it
/// on restore via `KVM_SET_VCPU_EVENTS`, so a **non-quiescent** point — an interrupt in flight —
/// is now snapshottable rather than fail-closed-rejected. That is the whole point of this task.
/// **Two cap-gated event fields are the exception** (PR #12 round 7): `triple_fault_pending` and
/// `exception_has_payload` are rejected here, because their `KVM_SET_VCPU_EVENTS` validity bits
/// need per-VM capabilities (`KVM_CAP_X86_TRIPLE_FAULT_EVENT` / `KVM_CAP_EXCEPTION_PAYLOAD`) this
/// backend does not enable — so a captured value could not be restored. Rejecting at *save* keeps
/// the codec **provably lossless-or-rejected** and save/restore symmetric (see below). The other
/// remaining fail-closed fields are the PAE-only `sregs.flags`/`pdptrs` and `debugregs.flags`,
/// all zero for the 64-bit / paging-off determinism guest at any V-time point.
///
/// (Two further non-`VcpuState` gaps are handled at *restore*, not here: a non-empty
/// `timers` section is rejected, and a staged backend completion is **defined out** —
/// a snapshot is taken only at a clean, V-time-synchronized boundary with no staged
/// RNG completion; see [`crate::vmm::Vmm::save_vm_state`] / [`crate::vmm::Vmm::restore_vm_state`].)
///
/// Returns `None` for any representable point (quiescent **or** with an interrupt in
/// flight). Pure.
pub(crate) fn unrepresentable_state(vcpu: &vmm_backend::VcpuState) -> Option<&'static str> {
    let s = &vcpu.sregs;
    if s.flags != 0 {
        return Some(
            "kvm_sregs2 flags is set (e.g. PDPTRS_VALID) — the vm_state subset does not carry it; \
             the determinism guest is 64-bit / paging-off, so a snapshot here is unrepresentable",
        );
    }
    if s.pdptrs.iter().any(|&p| p != 0) {
        return Some(
            "PAE PDPTRs are non-zero — not carried by the vm_state subset (the determinism guest \
             is 64-bit / paging-off, where PDPTRs are unused)",
        );
    }
    if vcpu.debugregs.flags != 0 {
        return Some(
            "kvm_debugregs flags is set — the vm_state DebugRegs record carries DR0..3/DR6/DR7 but \
             not the flags field (KVM defines it as currently always 0)",
        );
    }
    // The full `kvm_vcpu_events` record IS captured now (device blob, task 41), so an in-flight
    // injection round-trips — except the two cap-gated fields KVM cannot restore here; see
    // [`cap_unrestorable_events`] (applied symmetrically at save and at restore-before-mutation).
    if let Some(reason) = cap_unrestorable_events(&vcpu.events) {
        return Some(reason);
    }
    None
}

/// Reason a `kvm_vcpu_events` record **cannot be restored on this backend** — it would set a
/// `KVM_SET_VCPU_EVENTS` validity bit gated behind a per-VM capability this backend does not
/// enable, so the SET ioctl returns `-EINVAL`. `None` if every set bit is restorable.
///
/// KVM rejects `VALID_TRIPLE_FAULT` / `VALID_PAYLOAD` on SET unless
/// `KVM_CAP_X86_TRIPLE_FAULT_EVENT` / `KVM_CAP_EXCEPTION_PAYLOAD` is enabled — **even with a
/// zero payload**. This backend enables neither (only `DETERMINISTIC_INTERCEPTS` +
/// `USER_SPACE_MSR`), and vmm-core cannot query per-cap state through the `Backend` trait, so
/// these fields are unrestorable. The check is on the **fields** that drive the rebuilt mask
/// (`triple_fault_pending → VALID_TRIPLE_FAULT`, `exception_has_payload → VALID_PAYLOAD`), so it
/// catches exactly the records whose [`events_for_restore`] would carry a cap-disabled bit.
///
/// Applied **symmetrically**: [`unrepresentable_state`] uses it so `save_vm_state` never seals an
/// unrestorable snapshot (save/restore symmetry, PR #12 round 7), and
/// [`crate::vmm::Vmm::restore_vm_state`] uses it to reject an untrusted/foreign `dev.events` blob
/// **before** any `Backend::restore` ioctl mutates the target vCPU — preserving restore's
/// reject-before-mutation (atomic) contract (PR #12 round 8). A real `KVM_GET` on this backend
/// never reports either field (a triple fault is a `KVM_EXIT_SHUTDOWN`; with the payload cap off
/// KVM folds the payload via the legacy path, leaving `has_payload = 0`), so this never rejects a
/// genuine captured point — it closes the contract for a synthetic / relayed / forward-compat blob.
pub(crate) fn cap_unrestorable_events(e: &vmm_backend::VcpuEvents) -> Option<&'static str> {
    if e.triple_fault_pending != 0 {
        return Some(
            "kvm_vcpu_events.triple_fault_pending is set, but KVM_CAP_X86_TRIPLE_FAULT_EVENT is not \
             enabled on this backend — KVM_SET_VCPU_EVENTS would reject it (-EINVAL); fail closed \
             rather than seal/restore an unrestorable snapshot",
        );
    }
    if e.exception_has_payload != 0 {
        return Some(
            "kvm_vcpu_events.exception_has_payload is set, but KVM_CAP_EXCEPTION_PAYLOAD is not \
             enabled on this backend — KVM_SET_VCPU_EVENTS would reject it (-EINVAL); fail closed \
             rather than seal/restore an unrestorable snapshot",
        );
    }
    None
}

/// `true` iff `e` carries `kvm_vcpu_events` state **the quiescent-only task-39 codec
/// fail-closed-rejected** (`unrepresentable_state`'s old 14-field check) — the exact
/// predicate that decided "non-quiescent, refuse." It fires on a *genuine* in-flight
/// injection (an interrupt/exception KVM has injected but not yet delivered, the
/// `#PF`/`#DB` payload, a `SIPI`, SMM, or a queued triple fault) **and** on KVM's inert
/// *modifier residuals* — a stale `interrupt.nr`/`exception.nr`/`has_error_code` KVM
/// leaves set after an injection completes (box evidence: the post-readiness Postgres
/// boundaries it flagged carried such residuals, the active bits all clear). Both made
/// task 39 refuse; task 41 makes both snapshottable — the residuals collapse to the
/// clean record under [`canonical_events`], a true injection round-trips. Excluded
/// (never a refusal trigger): `exception_pending`/`exception_nr`/`exception_error_code`/
/// `nmi_pending`/`smi_pending`/`interrupt_shadow` and the validity-mask `flags`. Pure;
/// exposed via [`crate::vmm::Vmm::has_inflight_event_injection`] so a gate can quote a
/// run's task-39-would-reject split.
pub(crate) fn has_inflight_injection(e: &vmm_backend::VcpuEvents) -> bool {
    let fields: [u64; 14] = [
        u64::from(e.exception_injected),
        u64::from(e.exception_has_error_code),
        u64::from(e.exception_has_payload),
        e.exception_payload,
        u64::from(e.interrupt_injected),
        u64::from(e.interrupt_nr),
        u64::from(e.interrupt_soft),
        u64::from(e.nmi_injected),
        u64::from(e.nmi_masked),
        u64::from(e.sipi_vector),
        u64::from(e.smi_smm),
        u64::from(e.smi_inside_nmi),
        u64::from(e.smi_latched_init),
        u64::from(e.triple_fault_pending),
    ];
    fields.iter().any(|&x| x != 0)
}

/// `true` iff `e` carries a **genuine in-flight event** — a real injected-or-pending bit,
/// the *active* subset of [`has_inflight_injection`].
///
/// Where [`has_inflight_injection`] fires on KVM's inert **modifier residuals** too (a
/// stale `interrupt.nr` / `exception.has_error_code` / `sipi_vector` left set with every
/// active bit clear), this fires **only** when an event is actually mid-flight: an
/// injected interrupt / exception / NMI, a pending exception / NMI / SMI, a queued triple
/// fault, or a valid SIPI. A residual is *not* a non-quiescent point — it collapses to the
/// clean quiescent record under [`canonical_events`] — so a gate that wants to **prove** a
/// non-quiescent snapshot (an event KVM committed to that the guest has not yet consumed)
/// must seal on **this**, not on `has_inflight_injection` (which would let an inert
/// residual seal a quiescent point dressed as non-quiescent). On a real
/// `KVM_GET_VCPU_EVENTS` the SIPI vector is reported 0 with `VALID_SIPI_VECTOR` clear (it
/// is SET-only), so the SIPI term never fires for a captured snapshot; it is kept for
/// completeness and to match [`canonical_events`]'s validity-bit-driven SIPI handling.
pub(crate) fn has_active_event_injection(e: &vmm_backend::VcpuEvents) -> bool {
    e.interrupt_injected != 0
        || e.exception_injected != 0
        || e.exception_pending != 0
        || e.nmi_injected != 0
        || e.nmi_pending != 0
        || e.smi_pending != 0
        || e.triple_fault_pending != 0
        || e.flags & KVM_VCPUEVENT_VALID_SIPI_VECTOR != 0
}

// ---------------------------------------------------------------------------
// The vmm-core device blob: the bytes carried in `vm_state::DeviceBlob`.
//
// `vm-state`'s typed records have no home for the userspace xAPIC, the 8259 IMRs,
// the PCI CONFIG_ADDRESS latch, the 8250 UART (registers + serial capture), or
// `IA32_TSC_ADJUST`. INTEGRATION.md §4 places all of those in the snapshot, and
// task 09 carries the device section as an opaque, length-delimited placeholder
// "the vmm-core adapter passes through whatever the device models emit". This is
// that emission: a small, versioned, little-endian TLV that vmm-core owns end to
// end (the codec never interprets it). Total decode, no panic (rule #4).
// ---------------------------------------------------------------------------

/// Device-blob magic: `"DEV1"` read little-endian.
const DEVICE_BLOB_MAGIC: u32 = 0x3156_4544;
/// Device-blob layout version. Bump on any layout change (independent of
/// `VM_STATE_VERSION`, since this lives inside the opaque device section).
/// v2 added the ordered conformance `report_stream`; v3 added the **full
/// `kvm_vcpu_events`** record (task 41 — non-quiescent capture, so an in-flight
/// interrupt/exception injection round-trips instead of being fail-closed-rejected).
const DEVICE_BLOB_VERSION: u16 = 3;

/// The 8250 UART residual state a snapshot carries: the serial capture buffer (so a
/// restored continuation reproduces byte-identical console output), the eight
/// register shadows, the latched `LCR.DLAB` window, and the divisor-latch-high byte.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct UartState {
    pub capture: Vec<u8>,
    pub regs: [u8; 8],
    pub dlab: bool,
    pub dlm: u8,
}

/// The legacy-platform residual state: the PCI CONFIG_ADDRESS latch and the two
/// 8259 IMRs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct LegacyState {
    pub config_address: u32,
    pub master_imr: u8,
    pub slave_imr: u8,
}

/// Everything the vmm-core device blob carries.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub(crate) struct DeviceState {
    /// `IA32_TSC_ADJUST` — the signed V-time TSC offset (no typed `vm_state` field).
    pub tsc_adjust: u64,
    /// The ordered conformance report stream (`REPORT_PORT` writes) — guest-
    /// observable output that feeds `observable_digest` (O2), restored so a branch
    /// resumes it. Empty for runs that never touch the report channel.
    pub report_stream: Vec<u32>,
    /// The 8250 UART residual state.
    pub uart: UartState,
    /// The userspace xAPIC register file + timer bookkeeping (Linux path only).
    pub lapic: Option<LapicState>,
    /// The legacy PC platform latches (Linux path only).
    pub legacy: Option<LegacyState>,
    /// The **full** `kvm_vcpu_events` (`KVM_GET_VCPU_EVENTS`) — every in-flight
    /// injection / interrupt-shadow / NMI / SMI / triple-fault field, not the reduced
    /// `vm_state::VcpuEvents` subset (task 41). This is what makes a **non-quiescent**
    /// V-time point snapshottable: an interrupt or exception KVM has injected but not
    /// yet delivered (`interrupt.injected` / `exception.injected` / the `#PF`/`#DB`
    /// payload) is captured here and re-established on restore via `KVM_SET_VCPU_EVENTS`,
    /// so the guest resumes mid-delivery identically. Zero at a quiescent point (so
    /// M1/M2/corpus blobs carry an all-zero record and their hashes do not move). The
    /// authoritative events on restore — it supersedes the reduced typed record, which
    /// `vm-state` still carries unchanged for task-39 codec compatibility.
    pub events: vmm_backend::VcpuEvents,
}

fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Append the full [`vmm_backend::VcpuEvents`] in fixed declaration order (all POD;
/// reversed by [`Reader::events`]). The byte order matches `vmm::encode_events` (the
/// `state_hash` event encoding), so the two never disagree on the event field set.
fn put_events(out: &mut Vec<u8>, e: &vmm_backend::VcpuEvents) {
    out.extend_from_slice(&[
        e.exception_injected,
        e.exception_nr,
        e.exception_has_error_code,
        e.exception_pending,
    ]);
    put_u32(out, e.exception_error_code);
    out.push(e.exception_has_payload);
    put_u64(out, e.exception_payload);
    out.extend_from_slice(&[
        e.interrupt_injected,
        e.interrupt_nr,
        e.interrupt_soft,
        e.interrupt_shadow,
        e.nmi_injected,
        e.nmi_pending,
        e.nmi_masked,
    ]);
    put_u32(out, e.sipi_vector);
    put_u32(out, e.flags);
    out.extend_from_slice(&[
        e.smi_smm,
        e.smi_pending,
        e.smi_inside_nmi,
        e.smi_latched_init,
        e.triple_fault_pending,
    ]);
}

/// Append a [`LapicState`] in fixed declaration order (all POD; reversed by
/// [`Reader::lapic`]).
fn put_lapic(out: &mut Vec<u8>, s: &LapicState) {
    put_u32(out, s.version);
    put_u32(out, s.id);
    put_u64(out, s.timer_hz);
    for x in [
        s.tpr,
        s.svr,
        s.ldr,
        s.dfr,
        s.esr,
        s.icr_low,
        s.icr_high,
        s.divide_config,
    ] {
        put_u32(out, x);
    }
    for word in s.isr.iter().chain(&s.tmr).chain(&s.irr).chain(&s.lvt) {
        put_u32(out, *word);
    }
    put_u32(out, s.initial_count);
    put_u32(out, s.count_at_arm);
    put_u64(out, s.timer_arm_vns);
    out.push(u8::from(s.timer_running));
    out.push(u8::from(s.timer_pending));
}

/// Encode a [`DeviceState`] into the vmm-core device blob (the `DeviceBlob` bytes).
pub(crate) fn encode_device_blob(d: &DeviceState) -> DeviceBlob {
    let mut out = Vec::new();
    put_u32(&mut out, DEVICE_BLOB_MAGIC);
    put_u16(&mut out, DEVICE_BLOB_VERSION);
    put_u64(&mut out, d.tsc_adjust);
    put_u32(&mut out, d.report_stream.len() as u32);
    for &word in &d.report_stream {
        put_u32(&mut out, word);
    }
    put_u32(&mut out, d.uart.capture.len() as u32);
    out.extend_from_slice(&d.uart.capture);
    out.extend_from_slice(&d.uart.regs);
    out.push(u8::from(d.uart.dlab));
    out.push(d.uart.dlm);
    match &d.lapic {
        Some(l) => {
            out.push(1);
            put_lapic(&mut out, l);
        }
        None => out.push(0),
    }
    match &d.legacy {
        Some(l) => {
            out.push(1);
            put_u32(&mut out, l.config_address);
            out.push(l.master_imr);
            out.push(l.slave_imr);
        }
        None => out.push(0),
    }
    // The full kvm_vcpu_events (task 41) — last, a fixed-width trailing record so the
    // earlier field offsets are unchanged from v2.
    put_events(&mut out, &d.events);
    DeviceBlob(out)
}

/// A total little-endian cursor over the device blob; every read is bounds-checked
/// and yields `None` past end-of-buffer (mapped to [`SnapshotError::DeviceBlob`]).
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        let b = self.take(2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Option<u64> {
        let b = self.take(8)?;
        Some(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn bool(&mut self) -> Option<bool> {
        match self.u8()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
    fn array8_u32(&mut self) -> Option<[u32; 8]> {
        let mut a = [0u32; 8];
        for slot in &mut a {
            *slot = self.u32()?;
        }
        Some(a)
    }
    fn array6_u32(&mut self) -> Option<[u32; 6]> {
        let mut a = [0u32; 6];
        for slot in &mut a {
            *slot = self.u32()?;
        }
        Some(a)
    }
    /// Reverse of [`put_events`]: the full `kvm_vcpu_events` in declaration order.
    fn events(&mut self) -> Option<vmm_backend::VcpuEvents> {
        Some(vmm_backend::VcpuEvents {
            exception_injected: self.u8()?,
            exception_nr: self.u8()?,
            exception_has_error_code: self.u8()?,
            exception_pending: self.u8()?,
            exception_error_code: self.u32()?,
            exception_has_payload: self.u8()?,
            exception_payload: self.u64()?,
            interrupt_injected: self.u8()?,
            interrupt_nr: self.u8()?,
            interrupt_soft: self.u8()?,
            interrupt_shadow: self.u8()?,
            nmi_injected: self.u8()?,
            nmi_pending: self.u8()?,
            nmi_masked: self.u8()?,
            sipi_vector: self.u32()?,
            flags: self.u32()?,
            smi_smm: self.u8()?,
            smi_pending: self.u8()?,
            smi_inside_nmi: self.u8()?,
            smi_latched_init: self.u8()?,
            triple_fault_pending: self.u8()?,
        })
    }
    fn lapic(&mut self) -> Option<LapicState> {
        Some(LapicState {
            version: self.u32()?,
            id: self.u32()?,
            timer_hz: self.u64()?,
            tpr: self.u32()?,
            svr: self.u32()?,
            ldr: self.u32()?,
            dfr: self.u32()?,
            esr: self.u32()?,
            icr_low: self.u32()?,
            icr_high: self.u32()?,
            divide_config: self.u32()?,
            isr: self.array8_u32()?,
            tmr: self.array8_u32()?,
            irr: self.array8_u32()?,
            lvt: self.array6_u32()?,
            initial_count: self.u32()?,
            count_at_arm: self.u32()?,
            timer_arm_vns: self.u64()?,
            timer_running: self.bool()?,
            timer_pending: self.bool()?,
        })
    }
}

/// Decode the vmm-core device blob. Strict and total: any malformation is a
/// [`SnapshotError::DeviceBlob`], never a panic.
pub(crate) fn decode_device_blob(blob: &[u8]) -> Result<DeviceState, SnapshotError> {
    let mut r = Reader::new(blob);
    let bad = |m: &'static str| SnapshotError::DeviceBlob(m);
    if r.u32().ok_or(bad("truncated header"))? != DEVICE_BLOB_MAGIC {
        return Err(bad("bad magic"));
    }
    if r.u16().ok_or(bad("truncated version"))? != DEVICE_BLOB_VERSION {
        return Err(bad("unsupported version"));
    }
    let tsc_adjust = r.u64().ok_or(bad("truncated tsc_adjust"))?;
    let report_len = r.u32().ok_or(bad("truncated report len"))? as usize;
    let mut report_stream = Vec::with_capacity(report_len.min(1 << 16));
    for _ in 0..report_len {
        report_stream.push(r.u32().ok_or(bad("truncated report stream"))?);
    }
    let cap_len = r.u32().ok_or(bad("truncated capture len"))? as usize;
    let capture = r.take(cap_len).ok_or(bad("truncated capture"))?.to_vec();
    let regs: [u8; 8] = r
        .take(8)
        .ok_or(bad("truncated uart regs"))?
        .try_into()
        .map_err(|_| bad("uart regs len"))?;
    let dlab = r.bool().ok_or(bad("bad uart dlab"))?;
    let dlm = r.u8().ok_or(bad("truncated uart dlm"))?;
    let lapic = match r.u8().ok_or(bad("truncated lapic flag"))? {
        0 => None,
        1 => Some(r.lapic().ok_or(bad("truncated lapic"))?),
        _ => return Err(bad("bad lapic flag")),
    };
    let legacy = match r.u8().ok_or(bad("truncated legacy flag"))? {
        0 => None,
        1 => Some(LegacyState {
            config_address: r.u32().ok_or(bad("truncated legacy addr"))?,
            master_imr: r.u8().ok_or(bad("truncated master imr"))?,
            slave_imr: r.u8().ok_or(bad("truncated slave imr"))?,
        }),
        _ => return Err(bad("bad legacy flag")),
    };
    let events = r.events().ok_or(bad("truncated vcpu events"))?;
    if r.pos != blob.len() {
        return Err(bad("trailing bytes"));
    }
    Ok(DeviceState {
        tsc_adjust,
        report_stream,
        uart: UartState {
            capture,
            regs,
            dlab,
            dlm,
        },
        lapic,
        legacy,
        events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lapic_state(tpr: u32) -> LapicState {
        LapicState {
            version: lapic::LAPIC_STATE_VERSION,
            id: 0,
            timer_hz: 24_000_000,
            tpr,
            svr: 0x1FF,
            ldr: 0,
            dfr: 0xFFFF_FFFF,
            esr: 0,
            icr_low: 0,
            icr_high: 0,
            divide_config: 0,
            isr: [0; 8],
            tmr: [0; 8],
            irr: [0; 8],
            lvt: [0x1_0000, 0x1_0000, 0x1_0000, 0x1_0000, 0x1_0000, 0x1_0000],
            initial_count: 0,
            count_at_arm: 0,
            timer_arm_vns: 0,
            timer_running: false,
            timer_pending: false,
        }
    }

    // --- segment pack/unpack -------------------------------------------------

    #[test]
    fn segment_pack_unpack_round_trips_every_bit() {
        // Each present/dpl/s/db/l/g/avl bit set, on a **usable** segment (`unusable = 0`) so
        // its `type_` is carried verbatim — flipping any one pack/unpack shift moves a set
        // bit and fails the round-trip (no bit can be 0-and-invisible).
        let seg = vmm_backend::Segment {
            base: 0xDEAD_BEEF_0000_1000,
            limit: 0xFFFF_FFFF,
            selector: 0x1234,
            type_: 0xB,
            present: 1,
            dpl: 3,
            db: 1,
            s: 1,
            l: 1,
            g: 1,
            avl: 1,
            unusable: 0,
        };
        let packed = pack_segment(&seg);
        // present(1) | dpl(3)<<1 | s(1)<<3 = 1 | 6 | 8 = 0xF.
        assert_eq!(packed.present_dpl_s, 0x0F);
        // db(1) | l(1)<<1 | g(1)<<2 | avl(1)<<3 = 1|2|4|8 = 0xF (unusable = 0).
        assert_eq!(packed.flags, 0x0F);
        assert_eq!(
            packed.type_, 0xB,
            "a usable segment carries its type verbatim"
        );
        assert_eq!(unpack_segment(&packed), seg);
        // The `unusable` flag bit (`flags << 4`) exercised, AND its `type` canonicalized to
        // 0 (PR #12 round 4 — mirror `encode_segment`): an unusable segment with a non-zero
        // raw `type_` packs to `type 0`, so the VMST hash chunk matches a live `KVM_GET_SREGS`
        // (which reports `type 0`) and a `save → restore → save` does not diverge. The
        // round-trip is therefore to the **canonical** form, not the raw input.
        let unusable_raw = vmm_backend::Segment {
            type_: 5,
            unusable: 1,
            ..Default::default()
        };
        let pu = pack_segment(&unusable_raw);
        assert_eq!(pu.flags, 0x10, "the unusable bit packs to flags << 4");
        assert_eq!(
            pu.type_, 0,
            "an unusable segment's non-zero type canonicalizes to 0"
        );
        let unusable_canonical = vmm_backend::Segment {
            type_: 0,
            unusable: 1,
            ..Default::default()
        };
        assert_eq!(
            unpack_segment(&pu),
            unusable_canonical,
            "pack canonicalizes the unusable type; the round-trip is to the canonical form"
        );
        // A zero segment also round-trips (the all-1 case alone could mask a stuck-high bug).
        let zero = vmm_backend::Segment::default();
        assert_eq!(unpack_segment(&pack_segment(&zero)), zero);
    }

    #[test]
    fn segment_dpl_distinguishes_all_four_levels() {
        for dpl in 0u8..=3 {
            let seg = vmm_backend::Segment {
                dpl,
                ..Default::default()
            };
            assert_eq!(unpack_segment(&pack_segment(&seg)).dpl, dpl);
        }
    }

    // --- vcpu-state round-trip ----------------------------------------------

    fn sample_vcpu() -> vmm_backend::VcpuState {
        let mut msrs = std::collections::BTreeMap::new();
        msrs.insert(0xC000_0080u32, 0x501);
        msrs.insert(0x277u32, 0x0007_0406);
        vmm_backend::VcpuState {
            regs: vmm_backend::VcpuRegs {
                rax: 1,
                rbx: 2,
                rcx: 3,
                rdx: 4,
                rsi: 5,
                rdi: 6,
                rsp: 7,
                rbp: 8,
                r8: 9,
                r9: 10,
                r10: 11,
                r11: 12,
                r12: 13,
                r13: 14,
                r14: 15,
                r15: 16,
                rip: 0x10_0000,
                rflags: 0x2,
            },
            sregs: vmm_backend::VcpuSregs {
                cs: vmm_backend::Segment {
                    base: 0,
                    limit: 0xFFFF_FFFF,
                    selector: 0x10,
                    type_: 0xB,
                    present: 1,
                    dpl: 0,
                    db: 0,
                    s: 1,
                    l: 1,
                    g: 1,
                    avl: 0,
                    unusable: 0,
                },
                gdt: vmm_backend::DescriptorTable {
                    base: 0x6000,
                    limit: 0x27,
                },
                idt: vmm_backend::DescriptorTable { base: 0, limit: 0 },
                cr0: 0x8000_0011,
                cr3: 0x1000,
                cr4: 0x20,
                efer: 0x500,
                apic_base: 0xFEE0_0900,
                ..Default::default()
            },
            xcr0: 0x7,
            debugregs: vmm_backend::DebugRegs {
                db: [1, 2, 3, 4],
                dr6: 0xFFFF_0FF0,
                dr7: 0x400,
                flags: 0,
            },
            // A CANONICAL, typed-representable events record: the reduced `vm_state::VcpuEvents`
            // carries `{exception_pending, exception_vector, exception_error_code, nmi_pending,
            // smi_pending, interrupt_shadow}` only — it does NOT carry `exception_has_error_code`,
            // and `canonical_events` zeroes a value field whose validity bit is clear (round-8
            // audit). So a faithful round-trip uses `has_error_code = 0` ⇒ `error_code = 0`. (The
            // device blob — not this reduced record — carries a genuine error-coded exception on
            // restore; see `from_vm_events`.)
            events: vmm_backend::VcpuEvents {
                exception_pending: 1,
                exception_nr: 14,
                nmi_pending: 1,
                smi_pending: 1,
                interrupt_shadow: 1,
                ..Default::default()
            },
            mp_state: vmm_backend::MpState::Halted,
            msrs,
            xsave: (0u16..600).map(|i| i as u8).collect(),
        }
    }

    /// The representable-subset round-trip the snapshot path relies on: a live
    /// `VcpuState` whose dropped fields are zero (the quiescent-point invariant)
    /// survives `fill_vcpu_state` → `vcpu_state_from` byte-for-byte.
    #[test]
    fn vcpu_state_round_trips_through_vm_state() {
        let original = sample_vcpu();
        let mut s = VmState::default();
        fill_vcpu_state(&mut s, &original);
        let back = vcpu_state_from(&s);
        assert_eq!(back, original);
    }

    #[test]
    fn fill_vcpu_state_leaves_timer_queue_empty() {
        let mut s = VmState::default();
        fill_vcpu_state(&mut s, &sample_vcpu());
        assert_eq!(s.timers, TimerQueueState::default());
        assert_eq!(s.msrs.0.len(), 2, "both allow-stateful MSRs carried");
        assert_eq!(s.xcrs.xcr0, 0x7);
        assert_eq!(s.mp_state, MpState::Halted);
    }

    #[test]
    fn fill_vcpu_state_canonicalizes_the_typed_events_record() {
        // P2 (PR #12): the reduced typed `vm_state::VcpuEvents` must carry the CANONICAL
        // events — an inert residual (a stale exception vector/error-code with neither
        // `injected` nor `pending`) must collapse, mirroring the device blob. Otherwise,
        // with `wire_snapshot_hashing()` ON, the raw residual rides the VMST hash chunk and
        // a save→restore→save round-trip's `state_hash` diverges from the source.
        let mut vcpu = sample_vcpu();
        vcpu.events = vmm_backend::VcpuEvents {
            exception_nr: 13, // residual: set, but...
            exception_error_code: 0xABCD,
            exception_has_error_code: 1,
            interrupt_nr: 0x34,   // residual: last-delivered vector, not injected
            flags: 0x0D,          // GET-only validity bits
            ..Default::default()  // injected / pending all 0 ⇒ all inert
        };
        let mut s = VmState::default();
        fill_vcpu_state(&mut s, &vcpu);
        // The typed record == the projection of the CANONICAL events (which zero the
        // inert residual), NOT the raw events.
        assert_eq!(s.events, to_vm_events(&canonical_events(&vcpu.events)));
        // Concretely: the residual exception vector / error-code collapsed.
        assert_eq!(
            s.events.exception_vector, 0,
            "inert exception vector collapsed"
        );
        assert_eq!(
            s.events.exception_error_code, 0,
            "inert error code collapsed"
        );
        assert!(!s.events.exception_pending);
    }

    // --- device blob ---------------------------------------------------------

    /// A full `kvm_vcpu_events` with **every** field set to a distinct non-zero value,
    /// so any encode/decode field that is dropped, reordered, or width-truncated fails
    /// the round-trip (task 41 — the non-quiescent capture).
    fn full_events() -> vmm_backend::VcpuEvents {
        vmm_backend::VcpuEvents {
            exception_injected: 1,
            exception_nr: 14,
            exception_has_error_code: 1,
            exception_pending: 1,
            exception_error_code: 0xDEAD_BEEF,
            exception_has_payload: 1,
            exception_payload: 0x1234_5678_9ABC_DEF0,
            interrupt_injected: 1,
            interrupt_nr: 0x34,
            interrupt_soft: 1,
            interrupt_shadow: 3,
            nmi_injected: 1,
            nmi_pending: 1,
            nmi_masked: 1,
            sipi_vector: 0x00AB_00CD,
            flags: 0x0000_001F,
            smi_smm: 1,
            smi_pending: 1,
            smi_inside_nmi: 1,
            smi_latched_init: 1,
            triple_fault_pending: 1,
        }
    }

    #[test]
    fn device_blob_round_trips_all_fields() {
        let d = DeviceState {
            tsc_adjust: 0xCAFE_F00D_1234_5678,
            report_stream: vec![0x1111_1111, 0x0000_0000, 0xDEAD_BEEF],
            uart: UartState {
                capture: b"GUEST_READY PASS\n".to_vec(),
                regs: [0x01, 0x02, 0xC7, 0x03, 0x03, 0x00, 0x00, 0x00],
                dlab: true,
                dlm: 0x09,
            },
            lapic: Some(lapic_state(0x20)),
            legacy: Some(LegacyState {
                config_address: 0x8000_1000,
                master_imr: 0xEF,
                slave_imr: 0xFF,
            }),
            events: full_events(),
        };
        let blob = encode_device_blob(&d);
        let decoded = decode_device_blob(&blob.0).unwrap();
        assert_eq!(decoded, d);
        // The report stream survives in execution order (not reordered/dropped).
        assert_eq!(decoded.report_stream, vec![0x1111_1111, 0, 0xDEAD_BEEF]);
    }

    #[test]
    fn device_blob_round_trips_a_full_in_flight_events_record() {
        // Task 41: the *whole* kvm_vcpu_events — every in-flight injection field, the
        // #PF/#DB payload, SMM, triple-fault — round-trips through the device blob,
        // field for field. (Quiescent capture would have zeroed all but a 6-field
        // subset.) Each field is distinct + non-zero, so a dropped/aliased field fails.
        let d = DeviceState {
            events: full_events(),
            ..Default::default()
        };
        let decoded = decode_device_blob(&encode_device_blob(&d).0).unwrap();
        assert_eq!(
            decoded.events,
            full_events(),
            "the full in-flight kvm_vcpu_events must round-trip every field"
        );
        // A default (quiescent, all-zero) events record is the M1/M2/corpus case.
        let zero = DeviceState::default();
        assert_eq!(
            decode_device_blob(&encode_device_blob(&zero).0)
                .unwrap()
                .events,
            vmm_backend::VcpuEvents::default()
        );
    }

    #[test]
    fn has_inflight_injection_flags_exactly_the_non_quiescent_fields() {
        // A quiescent record (default, plus the always-representable subset) is NOT
        // in-flight; each in-flight field alone IS. Pins the exact field set so a mutant
        // dropping any single field (the gate-1 before/after measurement depends on it)
        // is caught.
        assert!(!has_inflight_injection(&vmm_backend::VcpuEvents::default()));
        // The captured/excluded subset must NOT count as in-flight.
        for excluded in [
            vmm_backend::VcpuEvents {
                exception_pending: 1,
                ..Default::default()
            },
            vmm_backend::VcpuEvents {
                exception_nr: 14,
                ..Default::default()
            },
            vmm_backend::VcpuEvents {
                exception_error_code: 0xFFFF,
                ..Default::default()
            },
            vmm_backend::VcpuEvents {
                nmi_pending: 1,
                ..Default::default()
            },
            vmm_backend::VcpuEvents {
                smi_pending: 1,
                ..Default::default()
            },
            vmm_backend::VcpuEvents {
                interrupt_shadow: 1,
                ..Default::default()
            },
            vmm_backend::VcpuEvents {
                flags: 0x1F,
                ..Default::default()
            },
        ] {
            assert!(
                !has_inflight_injection(&excluded),
                "an always-representable field must not mark a non-quiescent point: {excluded:?}"
            );
        }
        // Each of the 14 in-flight fields alone marks a non-quiescent point.
        let mut probes: Vec<vmm_backend::VcpuEvents> = Vec::new();
        for set in [
            |e: &mut vmm_backend::VcpuEvents| e.exception_injected = 1,
            |e: &mut vmm_backend::VcpuEvents| e.exception_has_error_code = 1,
            |e: &mut vmm_backend::VcpuEvents| e.exception_has_payload = 1,
            |e: &mut vmm_backend::VcpuEvents| e.exception_payload = 0xCAFE,
            |e: &mut vmm_backend::VcpuEvents| e.interrupt_injected = 1,
            |e: &mut vmm_backend::VcpuEvents| e.interrupt_nr = 0x34,
            |e: &mut vmm_backend::VcpuEvents| e.interrupt_soft = 1,
            |e: &mut vmm_backend::VcpuEvents| e.nmi_injected = 1,
            |e: &mut vmm_backend::VcpuEvents| e.nmi_masked = 1,
            |e: &mut vmm_backend::VcpuEvents| e.sipi_vector = 0xAB,
            |e: &mut vmm_backend::VcpuEvents| e.smi_smm = 1,
            |e: &mut vmm_backend::VcpuEvents| e.smi_inside_nmi = 1,
            |e: &mut vmm_backend::VcpuEvents| e.smi_latched_init = 1,
            |e: &mut vmm_backend::VcpuEvents| e.triple_fault_pending = 1,
        ] {
            let mut e = vmm_backend::VcpuEvents::default();
            set(&mut e);
            probes.push(e);
        }
        for e in probes {
            assert!(
                has_inflight_injection(&e),
                "an in-flight field must mark a non-quiescent point: {e:?}"
            );
        }
    }

    #[test]
    fn has_active_event_injection_flags_only_genuine_injections_not_residuals() {
        // The *active* subset of has_inflight_injection: only a real injected/pending bit
        // counts, never an inert modifier residual. The live gate seals on THIS, so the
        // active/residual split must be exact — a `|| → &&` on any operand (which would
        // stop a single genuine event from sealing) and a stray residual term (which would
        // let a quiescent-dressed residual seal a non-headline point) are both caught.
        assert!(!has_active_event_injection(
            &vmm_backend::VcpuEvents::default()
        ));
        // Each GENUINE active bit alone marks an in-flight event (one operand of the chain).
        for set in [
            |e: &mut vmm_backend::VcpuEvents| e.interrupt_injected = 1,
            |e: &mut vmm_backend::VcpuEvents| e.exception_injected = 1,
            |e: &mut vmm_backend::VcpuEvents| e.exception_pending = 1,
            |e: &mut vmm_backend::VcpuEvents| e.nmi_injected = 1,
            |e: &mut vmm_backend::VcpuEvents| e.nmi_pending = 1,
            |e: &mut vmm_backend::VcpuEvents| e.smi_pending = 1,
            |e: &mut vmm_backend::VcpuEvents| e.triple_fault_pending = 1,
            |e: &mut vmm_backend::VcpuEvents| e.flags = KVM_VCPUEVENT_VALID_SIPI_VECTOR,
        ] {
            let mut e = vmm_backend::VcpuEvents::default();
            set(&mut e);
            assert!(
                has_active_event_injection(&e),
                "a genuine injected/pending bit must mark an active event: {e:?}"
            );
        }
        // Every inert modifier residual alone must NOT (each collapses under
        // canonical_events). This is the whole point of the active/residual distinction:
        // a residual is snapshottable but does not prove a non-quiescent point. A nonzero
        // `sipi_vector` with VALID_SIPI_VECTOR clear is a residual (the SIPI edge fix).
        for set in [
            |e: &mut vmm_backend::VcpuEvents| e.interrupt_nr = 0x34,
            |e: &mut vmm_backend::VcpuEvents| e.interrupt_soft = 1,
            |e: &mut vmm_backend::VcpuEvents| e.exception_nr = 14,
            |e: &mut vmm_backend::VcpuEvents| e.exception_has_error_code = 1,
            |e: &mut vmm_backend::VcpuEvents| e.exception_error_code = 0xFFFF,
            |e: &mut vmm_backend::VcpuEvents| e.exception_has_payload = 1,
            |e: &mut vmm_backend::VcpuEvents| e.exception_payload = 0xCAFE,
            |e: &mut vmm_backend::VcpuEvents| e.nmi_masked = 1,
            |e: &mut vmm_backend::VcpuEvents| e.interrupt_shadow = 1,
            |e: &mut vmm_backend::VcpuEvents| e.sipi_vector = 0xAB, // bit clear → residual
            |e: &mut vmm_backend::VcpuEvents| e.smi_smm = 1,
            |e: &mut vmm_backend::VcpuEvents| e.smi_inside_nmi = 1,
            |e: &mut vmm_backend::VcpuEvents| e.smi_latched_init = 1,
        ] {
            let mut e = vmm_backend::VcpuEvents::default();
            set(&mut e);
            assert!(
                !has_active_event_injection(&e),
                "an inert modifier residual must NOT mark an active event: {e:?}"
            );
        }
    }

    #[test]
    fn canonical_events_collapses_residuals_and_reconstructs_flags() {
        // The box bug: KVM leaves inert modifier residuals (a stale interrupt.nr /
        // exception.nr/has_error_code, the GET-only validity bits) set even at a
        // quiescent point; replaying them raw into KVM_SET_VCPU_EVENTS corrupts the
        // resumed guest. They must collapse to the clean record.
        let residual = vmm_backend::VcpuEvents {
            exception_nr: 13,
            exception_has_error_code: 1,
            interrupt_nr: 52,
            flags: 0x0D, // VALID_NMI_PENDING|SHADOW|SMM with all-zero fields (GET-side)
            ..Default::default()
        };
        assert_eq!(
            canonical_events(&residual),
            vmm_backend::VcpuEvents::default(),
            "inert modifier residuals must collapse to the clean quiescent record"
        );

        // A genuine in-flight injection round-trips, with the SET-meaning flags rebuilt
        // from the surviving fields.
        let injected = vmm_backend::VcpuEvents {
            interrupt_injected: 1,
            interrupt_nr: 0x34,
            interrupt_soft: 1,
            exception_injected: 1,
            exception_nr: 14,
            exception_has_error_code: 1,
            exception_error_code: 4,
            exception_has_payload: 1,
            exception_payload: 0xCAFE,
            interrupt_shadow: 1,
            nmi_masked: 1,
            triple_fault_pending: 1,
            // Every GET-side flag bit set EXCEPT VALID_SIPI_VECTOR (whose validity is now
            // preserved from this bit, not inferred — tested separately). The rest are
            // ignored and rebuilt from the surviving fields below.
            flags: !KVM_VCPUEVENT_VALID_SIPI_VECTOR,
            ..Default::default()
        };
        let c = canonical_events(&injected);
        assert_eq!(c.interrupt_injected, 1);
        assert_eq!(c.interrupt_nr, 0x34);
        assert_eq!(c.interrupt_soft, 1);
        assert_eq!(c.exception_injected, 1);
        assert_eq!(c.exception_payload, 0xCAFE);
        assert_eq!(c.triple_fault_pending, 1);
        // flags rebuilt: NMI_PENDING(nmi_masked) | SHADOW | PAYLOAD | TRIPLE_FAULT.
        assert_eq!(
            c.flags,
            KVM_VCPUEVENT_VALID_NMI_PENDING
                | KVM_VCPUEVENT_VALID_SHADOW
                | KVM_VCPUEVENT_VALID_PAYLOAD
                | KVM_VCPUEVENT_VALID_TRIPLE_FAULT
        );
        // Idempotent — canonicalizing twice is a no-op (a re-saved restore reproduces).
        assert_eq!(
            canonical_events(&c),
            c,
            "canonical_events must be idempotent"
        );

        // Interrupt/exception modifiers drop when no injection is active; SIPI/SMM bits
        // appear only with their fields.
        let no_int = vmm_backend::VcpuEvents {
            interrupt_nr: 9,
            ..Default::default()
        };
        assert_eq!(canonical_events(&no_int).interrupt_nr, 0);
        // SIPI is gated on the *original* validity bit, never on `sipi_vector != 0`. KVM
        // zeroes the vector and clears the bit on every GET (it is SET-only), so this is
        // the general-correctness path, not the captured-snapshot path.
        //   * a nonzero vector with the bit CLEAR is a stale residual → drop vector + bit;
        let sipi_residual = vmm_backend::VcpuEvents {
            sipi_vector: 0xAB, // Default flags = 0 → VALID_SIPI_VECTOR clear
            ..Default::default()
        };
        let cr = canonical_events(&sipi_residual);
        assert_eq!(
            cr.sipi_vector, 0,
            "a SIPI residual (bit clear) drops the vector"
        );
        assert_eq!(
            cr.flags & KVM_VCPUEVENT_VALID_SIPI_VECTOR,
            0,
            "a SIPI residual (bit clear) clears the validity bit"
        );
        //   * a vector with the bit SET is genuine → carry vector + keep bit;
        let sipi_genuine = vmm_backend::VcpuEvents {
            sipi_vector: 0xAB,
            flags: KVM_VCPUEVENT_VALID_SIPI_VECTOR,
            ..Default::default()
        };
        let cg = canonical_events(&sipi_genuine);
        assert_eq!(cg.sipi_vector, 0xAB, "a valid SIPI carries its vector");
        assert_eq!(
            cg.flags & KVM_VCPUEVENT_VALID_SIPI_VECTOR,
            KVM_VCPUEVENT_VALID_SIPI_VECTOR,
            "a valid SIPI keeps the validity bit"
        );
        //   * vector 0 with the bit SET is a LEGAL SIPI (not a residual) → bit survives
        //     (a `sipi_vector != 0` test would have wrongly dropped it).
        let sipi_zero = vmm_backend::VcpuEvents {
            sipi_vector: 0,
            flags: KVM_VCPUEVENT_VALID_SIPI_VECTOR,
            ..Default::default()
        };
        assert_eq!(
            canonical_events(&sipi_zero).flags & KVM_VCPUEVENT_VALID_SIPI_VECTOR,
            KVM_VCPUEVENT_VALID_SIPI_VECTOR,
            "vector 0 with the validity bit set is a legal SIPI, not a residual"
        );
        // VALID_SMM is set by ANY of the four SMI sub-fields; VALID_NMI_PENDING by ANY
        // of the three NMI sub-fields. Exercise EACH operand alone so the OR-chains in
        // `canonical_events` are pinned (a `|| → &&` mutation on any operand is caught).
        for set_smm in [
            |e: &mut vmm_backend::VcpuEvents| e.smi_smm = 1,
            |e: &mut vmm_backend::VcpuEvents| e.smi_pending = 1,
            |e: &mut vmm_backend::VcpuEvents| e.smi_inside_nmi = 1,
            |e: &mut vmm_backend::VcpuEvents| e.smi_latched_init = 1,
        ] {
            let mut e = vmm_backend::VcpuEvents::default();
            set_smm(&mut e);
            assert_eq!(
                canonical_events(&e).flags & KVM_VCPUEVENT_VALID_SMM,
                KVM_VCPUEVENT_VALID_SMM,
                "each SMI sub-field alone sets VALID_SMM: {e:?}"
            );
        }
        for set_nmi in [
            |e: &mut vmm_backend::VcpuEvents| e.nmi_injected = 1,
            |e: &mut vmm_backend::VcpuEvents| e.nmi_pending = 1,
            |e: &mut vmm_backend::VcpuEvents| e.nmi_masked = 1,
        ] {
            let mut e = vmm_backend::VcpuEvents::default();
            set_nmi(&mut e);
            assert_eq!(
                canonical_events(&e).flags & KVM_VCPUEVENT_VALID_NMI_PENDING,
                KVM_VCPUEVENT_VALID_NMI_PENDING,
                "each NMI sub-field alone sets VALID_NMI_PENDING: {e:?}"
            );
        }
        // The all-quiescent record sets neither bit (the `&&`-degenerate baseline).
        let q = canonical_events(&vmm_backend::VcpuEvents::default());
        assert_eq!(q.flags & KVM_VCPUEVENT_VALID_SMM, 0);
        assert_eq!(q.flags & KVM_VCPUEVENT_VALID_NMI_PENDING, 0);
    }

    #[test]
    fn events_for_restore_clears_stale_target_state_regardless_of_freshness() {
        // PR #12 round 6 (codex/GPT-5.5) — restore must be independent of the prior occupant.
        // KVM treats a CLEAR validity bit on `KVM_SET_VCPU_EVENTS` as "leave that sub-record
        // UNCHANGED", so restoring a quiescent snapshot onto a NON-fresh vCPU (the branch /
        // restore-in-place case) would RETAIN its stale NMI-pending / interrupt-shadow / SMM /
        // triple-fault — a determinism leak. `events_for_restore` forces those validity bits ON
        // so restore explicitly clears that state.

        // Model `KVM_SET_VCPU_EVENTS`: a clear validity bit leaves the sub-record unchanged; the
        // always-applied fields (interrupt/exception/NMI injected + nmi.masked) overwrite.
        fn kvm_set(
            prev: &vmm_backend::VcpuEvents,
            set: &vmm_backend::VcpuEvents,
        ) -> vmm_backend::VcpuEvents {
            let mut out = *prev;
            // Unconditionally applied by KVM:
            out.interrupt_injected = set.interrupt_injected;
            out.interrupt_nr = set.interrupt_nr;
            out.interrupt_soft = set.interrupt_soft;
            out.exception_injected = set.exception_injected;
            out.exception_pending = set.exception_pending;
            out.exception_nr = set.exception_nr;
            out.exception_has_error_code = set.exception_has_error_code;
            out.exception_error_code = set.exception_error_code;
            out.nmi_injected = set.nmi_injected;
            out.nmi_masked = set.nmi_masked;
            // Validity-gated (a clear bit leaves the sub-record UNCHANGED):
            if set.flags & KVM_VCPUEVENT_VALID_NMI_PENDING != 0 {
                out.nmi_pending = set.nmi_pending;
            }
            if set.flags & KVM_VCPUEVENT_VALID_SHADOW != 0 {
                out.interrupt_shadow = set.interrupt_shadow;
            }
            if set.flags & KVM_VCPUEVENT_VALID_SMM != 0 {
                out.smi_smm = set.smi_smm;
                out.smi_pending = set.smi_pending;
                out.smi_inside_nmi = set.smi_inside_nmi;
                out.smi_latched_init = set.smi_latched_init;
            }
            if set.flags & KVM_VCPUEVENT_VALID_TRIPLE_FAULT != 0 {
                out.triple_fault_pending = set.triple_fault_pending;
            }
            if set.flags & KVM_VCPUEVENT_VALID_SIPI_VECTOR != 0 {
                out.sipi_vector = set.sipi_vector;
            }
            if set.flags & KVM_VCPUEVENT_VALID_PAYLOAD != 0 {
                out.exception_has_payload = set.exception_has_payload;
                out.exception_payload = set.exception_payload;
            }
            out
        }

        // A vCPU left dirty by a previous occupant; a clean snapshot has NONE of those. (No
        // stale triple-fault: KVM_CAP_X86_TRIPLE_FAULT_EVENT is not enabled by this backend, so
        // no triple-fault sub-record exists to leak — and its bit cannot be forced; see below.)
        let stale = vmm_backend::VcpuEvents {
            nmi_pending: 1,
            smi_smm: 1,
            smi_pending: 1,
            interrupt_shadow: 1,
            ..Default::default()
        };
        let clean = vmm_backend::VcpuEvents::default();

        // `events_for_restore` forces the CAP-FREE clear-on-restore bits ON (NMI_PENDING |
        // SHADOW | SMM), and ONLY those.
        let setv = events_for_restore(&clean);
        let force =
            KVM_VCPUEVENT_VALID_NMI_PENDING | KVM_VCPUEVENT_VALID_SHADOW | KVM_VCPUEVENT_VALID_SMM;
        assert_eq!(
            setv.flags & force,
            force,
            "events_for_restore sets NMI_PENDING|SHADOW|SMM unconditionally"
        );
        // TRIPLE_FAULT / PAYLOAD / SIPI stay gated — forcing TRIPLE_FAULT/PAYLOAD is -EINVAL
        // without their caps, SIPI is SET-only. For a quiescent snapshot all three are clear.
        assert_eq!(
            setv.flags & KVM_VCPUEVENT_VALID_TRIPLE_FAULT,
            0,
            "TRIPLE_FAULT is NOT forced (cap not enabled → would be -EINVAL)"
        );
        assert_eq!(
            setv.flags & KVM_VCPUEVENT_VALID_SIPI_VECTOR,
            0,
            "SIPI stays gated on restore"
        );
        assert_eq!(
            setv.flags & KVM_VCPUEVENT_VALID_PAYLOAD,
            0,
            "PAYLOAD stays gated on exception_has_payload"
        );

        // Restoring the clean snapshot onto the STALE vCPU clears every (cap-free) stale
        // sub-record, and yields the SAME result as restoring onto a fresh vCPU — restore is
        // target-independent for the bits this KVM lets us force.
        let restored_stale = kvm_set(&stale, &setv);
        let restored_fresh = kvm_set(&vmm_backend::VcpuEvents::default(), &setv);
        assert_eq!(restored_stale.nmi_pending, 0, "stale NMI-pending cleared");
        assert_eq!(restored_stale.smi_smm, 0, "stale SMM cleared");
        assert_eq!(restored_stale.smi_pending, 0, "stale SMI-pending cleared");
        assert_eq!(
            restored_stale.interrupt_shadow, 0,
            "stale interrupt-shadow cleared"
        );
        assert_eq!(
            restored_stale, restored_fresh,
            "restore is independent of the prior occupant (stale target == fresh target)"
        );

        // Contrast — the active-only `canonical_events` would LEAK the stale state (the exact
        // bug): its NMI_PENDING/SHADOW/SMM bits are clear for a quiescent record, so KVM leaves
        // the prior occupant's sub-records untouched.
        let leaked = kvm_set(&stale, &canonical_events(&clean));
        assert_ne!(
            leaked, restored_fresh,
            "canonical_events (gated bits) leaks the prior occupant's NMI/SMM/shadow"
        );
        assert_eq!(
            leaked.nmi_pending, 1,
            "the leak: stale NMI-pending survives the active-only mask"
        );

        // A genuine active injection still round-trips: events_for_restore preserves real state,
        // it only forces the validity bits + canonical (zero-when-inactive) payloads.
        let active = vmm_backend::VcpuEvents {
            nmi_injected: 1,
            nmi_pending: 1,
            interrupt_shadow: 1,
            ..Default::default()
        };
        let setv_active = events_for_restore(&active);
        assert_eq!(
            setv_active.nmi_pending, 1,
            "an active NMI-pending is preserved"
        );
        assert_eq!(
            setv_active.interrupt_shadow, 1,
            "an active interrupt-shadow is preserved"
        );
        assert_eq!(
            kvm_set(&stale, &setv_active).nmi_pending,
            1,
            "an active NMI-pending is applied on restore (not zeroed)"
        );
    }

    #[test]
    fn unrepresentable_state_fails_closed_on_cap_gated_event_fields() {
        // PR #12 round 7 — save/restore symmetry. `triple_fault_pending` and
        // `exception_has_payload` are the two `kvm_vcpu_events` fields whose
        // `KVM_SET_VCPU_EVENTS` validity bit needs a per-VM capability this backend does not
        // enable, so a captured value could NOT be restored (restore would be `-EINVAL`). Save
        // must fail closed on them — but NOT over-reject any restorable in-flight state.
        let representable = |events: vmm_backend::VcpuEvents| vmm_backend::VcpuState {
            events,
            ..Default::default()
        };
        // A quiescent point, and every restorable in-flight class, are representable (None).
        assert!(
            unrepresentable_state(&representable(vmm_backend::VcpuEvents::default())).is_none()
        );
        for ok in [
            vmm_backend::VcpuEvents {
                interrupt_injected: 1,
                interrupt_nr: 0x34,
                ..Default::default()
            },
            vmm_backend::VcpuEvents {
                exception_injected: 1,
                exception_nr: 13,
                exception_has_error_code: 1,
                exception_error_code: 0x18,
                ..Default::default()
            },
            vmm_backend::VcpuEvents {
                nmi_injected: 1,
                nmi_pending: 1,
                ..Default::default()
            },
            vmm_backend::VcpuEvents {
                interrupt_shadow: 1,
                ..Default::default()
            },
            vmm_backend::VcpuEvents {
                smi_smm: 1,
                ..Default::default()
            },
        ] {
            assert!(
                unrepresentable_state(&representable(ok)).is_none(),
                "a restorable in-flight field must NOT be rejected: {ok:?}"
            );
        }
        // The two cap-gated fields fail closed, each naming the offending field.
        let tf = unrepresentable_state(&representable(vmm_backend::VcpuEvents {
            triple_fault_pending: 1,
            ..Default::default()
        }))
        .expect("triple_fault_pending must fail closed at save");
        assert!(
            tf.contains("triple_fault_pending"),
            "reject reason names the field: {tf}"
        );
        let pl = unrepresentable_state(&representable(vmm_backend::VcpuEvents {
            exception_has_payload: 1,
            exception_payload: 0xCAFE,
            ..Default::default()
        }))
        .expect("exception_has_payload must fail closed at save");
        assert!(
            pl.contains("exception_has_payload"),
            "reject reason names the field: {pl}"
        );
    }

    #[test]
    fn canonical_events_zeroes_value_fields_when_their_validity_bit_is_clear() {
        // PR #12 round 8 — the class-closing audit. A VALUE field whose own validity bit is clear
        // is architecturally don't-care; it must be **zeroed** in the canonical form (mirror the
        // SIPI-vector gating), so a stale `error_code` / `payload` never reaches the canonical
        // blob or the `state_hash` (KVM would not apply it; replaying untrusted residual bytes
        // would diverge a save → restore → save). Exact-value, per the audit table in
        // IMPLEMENTATION.md.

        // exception_error_code is gated on exception_has_error_code:
        let stale_ec = canonical_events(&vmm_backend::VcpuEvents {
            exception_injected: 1,
            exception_nr: 13,
            exception_has_error_code: 0, // clear → error_code is don't-care
            exception_error_code: 0xDEAD_BEEF, // stale residual
            ..Default::default()
        });
        assert_eq!(
            stale_ec.exception_error_code, 0,
            "a stale error_code (has_error_code=0) is zeroed in the canonical form"
        );
        let valid_ec = canonical_events(&vmm_backend::VcpuEvents {
            exception_injected: 1,
            exception_nr: 13,
            exception_has_error_code: 1,
            exception_error_code: 0x18,
            ..Default::default()
        });
        assert_eq!(
            valid_ec.exception_error_code, 0x18,
            "a VALID error_code (has_error_code=1) is preserved"
        );

        // exception_payload is gated on exception_has_payload (a payload-bearing exception is
        // ALSO fail-closed-rejected at save/restore — this gates the canonical form / hash):
        let stale_pl = canonical_events(&vmm_backend::VcpuEvents {
            exception_injected: 1,
            exception_nr: 14,
            exception_has_payload: 0,       // clear → payload is don't-care
            exception_payload: 0xCAFE_F00D, // stale residual
            ..Default::default()
        });
        assert_eq!(
            stale_pl.exception_payload, 0,
            "a stale payload (has_payload=0) is zeroed in the canonical form"
        );
        let valid_pl = canonical_events(&vmm_backend::VcpuEvents {
            exception_injected: 1,
            exception_nr: 14,
            exception_has_payload: 1,
            exception_payload: 0xCAFE_F00D,
            ..Default::default()
        });
        assert_eq!(
            valid_pl.exception_payload, 0xCAFE_F00D,
            "a VALID payload (has_payload=1) is preserved in the canonical form"
        );

        // And both value fields are zero when no exception is injected/pending at all (the outer
        // gate), regardless of stale bytes.
        let no_exc = canonical_events(&vmm_backend::VcpuEvents {
            exception_error_code: 0xFF,
            exception_payload: 0xFF,
            exception_has_error_code: 1,
            exception_has_payload: 1,
            ..Default::default() // exception_injected = pending = 0
        });
        assert_eq!(
            no_exc.exception_error_code, 0,
            "no injected/pending exception → error_code 0"
        );
        assert_eq!(
            no_exc.exception_payload, 0,
            "no injected/pending exception → payload 0"
        );
    }

    #[test]
    fn device_blob_round_trips_without_optional_devices() {
        // M1/M2-style: no xAPIC, no legacy platform, no reports — just tsc_adjust + UART.
        let d = DeviceState {
            tsc_adjust: 0,
            report_stream: Vec::new(),
            uart: UartState {
                capture: b"PAYLOAD x PASS\n".to_vec(),
                regs: [0; 8],
                dlab: false,
                dlm: 0,
            },
            lapic: None,
            legacy: None,
            events: vmm_backend::VcpuEvents::default(),
        };
        let blob = encode_device_blob(&d);
        assert_eq!(decode_device_blob(&blob.0).unwrap(), d);
    }

    #[test]
    fn device_blob_decode_is_total_on_garbage() {
        // Truncations, a bad magic, and trailing bytes all yield a DeviceBlob error
        // (never a panic) — the rule-#4 fuzz-robustness discipline.
        assert!(matches!(
            decode_device_blob(&[]),
            Err(SnapshotError::DeviceBlob(_))
        ));
        assert!(matches!(
            decode_device_blob(&[0xFF, 0xFF, 0xFF, 0xFF, 1, 0]),
            Err(SnapshotError::DeviceBlob("bad magic"))
        ));
        let good = encode_device_blob(&DeviceState::default()).0;
        for cut in 0..good.len() {
            assert!(
                matches!(
                    decode_device_blob(&good[..cut]),
                    Err(SnapshotError::DeviceBlob(_))
                ),
                "truncation at {cut} must be a clean error"
            );
        }
        let mut trailing = good.clone();
        trailing.push(0xAB);
        assert!(matches!(
            decode_device_blob(&trailing),
            Err(SnapshotError::DeviceBlob("trailing bytes"))
        ));
    }

    #[test]
    fn device_blob_lapic_restores_through_lapic_crate() {
        // A decoded LapicState must be accepted by `lapic::Lapic::restore` — i.e. the
        // blob carries a *coherent* LapicState, not just round-trip bytes.
        let d = DeviceState {
            lapic: Some(lapic_state(0x10)),
            ..Default::default()
        };
        let decoded = decode_device_blob(&encode_device_blob(&d).0).unwrap();
        let ls = decoded.lapic.expect("lapic present");
        let restored = lapic::Lapic::restore(&ls).expect("coherent LapicState");
        assert_eq!(restored.snapshot(), ls);
    }

    // --- engine: base / derive / sharing ------------------------------------

    const PG: usize = PAGE_SIZE;

    fn img(pages: &[(usize, u8)], total_pages: usize) -> Vec<u8> {
        let mut m = vec![0u8; total_pages * PG];
        for &(gfn, byte) in pages {
            m[gfn * PG..(gfn + 1) * PG].fill(byte);
        }
        m
    }

    #[test]
    fn base_then_derive_stores_only_dirtied_pages() {
        let mut eng = SnapshotEngine::new(8 * PG);
        let base_mem = img(&[(0, 0xA), (1, 0xB), (5, 0xC)], 8);
        let base = eng.snapshot_base(&base_mem, b"base-blob").unwrap();
        assert_eq!(eng.stats(base).unwrap().owned_pages, 3);
        assert_eq!(eng.store_stats().stored_unique_pages, 3);

        // Dirty only page 1; the derive (full image, no dirty hint) must store ONE
        // owned page (the store's seal-time dedup drops the unchanged frames).
        let mut child_mem = base_mem.clone();
        child_mem[PG..2 * PG].fill(0xFF);
        let child = eng
            .snapshot_derive(base, &child_mem, None, b"child-blob")
            .unwrap();
        assert_eq!(
            eng.stats(child).unwrap().owned_pages,
            1,
            "derive is dirty-set-proportional even without a harvested dirty set"
        );
        // Store-wide: the 3 base contents + the 1 new content = 4 (page 1's old
        // 0xB is still referenced by the base).
        assert_eq!(eng.store_stats().stored_unique_pages, 4);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize uses mmap, which Miri cannot execute; the parse/convert logic is covered by the non-mmap tests"
    )]
    fn derive_with_dirty_hint_matches_full_capture() {
        let mut eng = SnapshotEngine::new(8 * PG);
        let base_mem = img(&[(0, 0xA), (3, 0xB)], 8);
        let base = eng.snapshot_base(&base_mem, b"b").unwrap();
        let mut mem = base_mem.clone();
        mem[3 * PG..4 * PG].fill(0x99);
        mem[7 * PG..8 * PG].fill(0x77);
        // Harvested dirty set {3, 7}: capture only those frames.
        let child = eng
            .snapshot_derive(base, &mem, Some(&[3, 7]), b"c")
            .unwrap();
        assert_eq!(eng.stats(child).unwrap().owned_pages, 2);
        // Materialize and confirm the dirtied frames read back the new content and
        // an untouched frame reads the base.
        let map = eng.materialize(child).unwrap();
        assert_eq!(map.as_slice()[3 * PG], 0x99);
        assert_eq!(map.as_slice()[7 * PG], 0x77);
        assert_eq!(map.as_slice()[0], 0xA);
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize uses mmap, which Miri cannot execute; the parse/convert logic is covered by the non-mmap tests"
    )]
    fn n_views_share_one_read_only_base() {
        // Gate 3: materialize N independent CoW views from one base; the base's
        // distinct contents are stored ONCE store-wide, not N×.
        let mut eng = SnapshotEngine::new(64 * PG);
        // 40 pages with DISTINCT non-zero content (byte i+1), so each is a distinct
        // store-wide content address (no incidental dedup masking the sharing claim).
        let base_mem = img(&(0..40).map(|i| (i, (i as u8) + 1)).collect::<Vec<_>>(), 64);
        let base = eng.snapshot_base(&base_mem, b"boot").unwrap();
        let unique_after_base = eng.store_stats().stored_unique_pages;
        assert_eq!(unique_after_base, 40);

        // Eight branches that each touch nothing: pure shared base.
        let mut views = Vec::new();
        for _ in 0..8 {
            let v = eng
                .snapshot_derive(base, &base_mem, Some(&[]), b"branch")
                .unwrap();
            views.push(eng.materialize(v).unwrap());
        }
        assert_eq!(
            eng.store_stats().stored_unique_pages,
            unique_after_base,
            "N branches that touched nothing add NO unique pages — the base is shared"
        );
        // Every view sees the same base image.
        for v in &views {
            assert_eq!(v.as_slice()[0], base_mem[0]);
            assert_eq!(v.as_slice()[39 * PG], base_mem[39 * PG]);
        }
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "materialize uses mmap, which Miri cannot execute; the parse/convert logic is covered by the non-mmap tests"
    )]
    fn materialize_reproduces_the_full_image() {
        let mut eng = SnapshotEngine::new(16 * PG);
        let mem = img(&[(0, 0x11), (8, 0x22), (15, 0x33)], 16);
        let base = eng.snapshot_base(&mem, b"x").unwrap();
        let map = eng.materialize(base).unwrap();
        assert_eq!(map.as_slice(), &mem[..]);
    }

    #[test]
    fn vm_state_blob_seals_and_decodes() {
        // The engine seals the canonical vm_state bytes and hands them back to decode.
        let mut eng = SnapshotEngine::new(4 * PG);
        let mut s = VmState {
            contract_hash: [7u8; 32],
            ..Default::default()
        };
        s.vtime.ratio_den = 1; // encodable
        let bytes = s.encode().unwrap();
        let snap = eng.snapshot_base(&vec![0u8; 4 * PG], &bytes).unwrap();
        assert_eq!(eng.vm_state(snap).unwrap(), s);
    }

    #[test]
    fn wrong_image_length_is_rejected() {
        let mut eng = SnapshotEngine::new(4 * PG);
        assert!(matches!(
            eng.snapshot_base(&vec![0u8; 3 * PG], b""),
            Err(SnapshotError::MemorySize { .. })
        ));
    }

    #[test]
    fn engine_mem_pages_retain_release_gc() {
        let mut eng = SnapshotEngine::new(8 * PG);
        assert_eq!(eng.mem_pages(), 8); // exact: kills mem_pages -> 0 / 1

        // One non-zero page + a non-empty blob, so gc has bytes to free.
        let mut mem = vec![0u8; 8 * PG];
        mem[..PG].fill(0xAB);
        let base = eng.snapshot_base(&mem, b"blob").unwrap(); // refcount 1
        assert_eq!(eng.store_stats().snapshots, 1);

        // retain → refcount 2; one release → still live (kills retain -> Ok(())).
        eng.retain(base).unwrap();
        eng.release(base).unwrap();
        assert_eq!(
            eng.store_stats().snapshots,
            1,
            "retain must have taken effect: one release of two refs leaves it live"
        );
        // Second release → refcount 0 (kills release -> Ok(())).
        eng.release(base).unwrap();
        assert_eq!(eng.store_stats().snapshots, 0, "released after both refs");

        // gc reaps the dead layer, freeing the one stored page + the 4-byte blob.
        // The exact value kills gc -> 0 and gc -> 1.
        assert_eq!(eng.gc(), PAGE_SIZE as u64 + 4);
    }

    #[test]
    fn out_of_range_dirty_gfn_is_rejected() {
        let mut eng = SnapshotEngine::new(4 * PG);
        let mem = vec![0u8; 4 * PG];
        let base = eng.snapshot_base(&mem, b"").unwrap();
        // gfn 4 is one past the 4-page (gfns 0..=3) image.
        assert!(matches!(
            eng.snapshot_derive(base, &mem, Some(&[4]), b""),
            Err(SnapshotError::DirtyGfnOutOfRange { gfn: 4, pages: 4 })
        ));
        // The in-range boundary gfn 3 is accepted.
        assert!(eng.snapshot_derive(base, &mem, Some(&[3]), b"").is_ok());
    }
}
