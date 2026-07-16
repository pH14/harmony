// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arm64 vendor's exit dispatch, interrupt-fabric seams, and snapshot
//! record glue (`docs/ARCH-BOUNDARY.md` §B's vendor column, arm64 row).
//!
//! Everything here names arm64: the PL011 device state, the `PSTATE.I`
//! interruptibility test, the sysreg-trap dispositions (fail-closed skeleton),
//! and the arm64 `vm_state` record set. The engine ([`crate::vmm`]) reaches
//! all of it **only** through the [`Vendor`](crate::vendor::Vendor) trait.
//!
//! **Skeleton posture, stated once:** no interrupt fabric is wired (the
//! `gicv3` model arrives with M2 and real *delivery* is `TODO(AA-6)` — the
//! vGICv3 round-trip verdict), no MMIO address is modeled (the machine memory
//! map arrives with the M3 boot path), and a trapped sysreg has no ruled
//! disposition (`TODO(AA-6)`). Every one of those absences **fails closed**,
//! never silently succeeds — default-deny is the posture the contract will
//! fill in, not a stub to be papered over.

use hypercall_proto::Service;
use vm_state::Arm64VmState;
use vmm_backend::{Arm64, Arm64VcpuState, Backend, Gpa};

use crate::snapshot::SnapshotError;
use crate::vendor::InterruptReject;
use crate::vendor::arm64::contract;
use crate::vendor::arm64::devices::Pl011;
use crate::vendor::arm64::records::{self, Arm64DeviceState};
use crate::vmm::{Step, Vmm, VmmError};

/// `PSTATE.I` (IRQ mask, bit 7): set ⇒ maskable interrupts are masked — the
/// arm64 mirror of x86's `RFLAGS_IF` (inverted sense: masked vs enabled).
pub(crate) const PSTATE_I: u64 = 1 << 7;

/// The arm64 per-VM device state
/// ([`Vendor::Devices`](crate::vendor::Vendor::Devices)): the PL011 UART
/// (always present — the serial console). The GICv3 + generic-timer fabric
/// joins as an optional wiring when the `gicv3` model lands (M2), mirroring
/// x86's `lapic: Option<_>`.
pub struct Arm64Devices {
    /// The PL011 UART (serial console + the task-81 `exec` input queue).
    pub(crate) uart: Pl011,
}

impl Arm64Devices {
    /// Fresh (reset) arm64 device state: a reset PL011, no fabric.
    pub(crate) fn new() -> Self {
        Self { uart: Pl011::new() }
    }
}

impl<B: Backend<A = Arm64>> Vmm<B> {
    /// Service a trapped sysreg access ([`Arm64Exit::Sysreg`]
    /// (`vmm_backend::Arm64Exit::Sysreg`)). **Fails closed:** the sysreg
    /// dispositions are the ARM CPU contract's rows (`TODO(AA-6)`, the
    /// enforcement-mechanism truth table) and the trap surface itself is the
    /// AA-3 patched backend's (`TODO(patched-abi)`) — the skeleton rules no
    /// disposition, so a surfaced trap is a loud contract violation, never a
    /// silently invented value or a silently dropped write.
    pub(crate) fn dispatch_sysreg(
        &mut self,
        sysreg: u32,
        write: Option<u64>,
    ) -> Result<Step, VmmError> {
        let dir = if write.is_some() { "write" } else { "read" };
        Err(VmmError::ContractViolation(format!(
            "trapped sysreg {dir} ({sysreg:#010x}) has no ruled disposition: the arm64 \
             contract's sysreg rows are the AA-6 truth table's (and the trap surface is the \
             AA-3 patched backend's) — the skeleton fails closed"
        )))
    }

    /// Route an MMIO access. **Fails closed for every address:** the arm64
    /// machine memory map (PL011 frame, GIC frames, the reserved hypercall
    /// doorbell GPA) is composed by the M3 boot path; until a composition
    /// wires it, no MMIO address is modeled — mirroring x86's posture when the
    /// xAPIC is unwired.
    pub(crate) fn dispatch_mmio_arm64(
        &mut self,
        gpa: Gpa,
        size: u8,
        write: Option<u64>,
    ) -> Result<Step, VmmError> {
        let _ = write;
        Err(VmmError::ContractViolation(format!(
            "unmodeled MMIO at {:#x} (size {size}); the arm64 skeleton wires no MMIO device \
             (the machine memory map lands with the boot path)",
            gpa.0
        )))
    }

    /// Advance the fabric and hand the backend the arbitrated deliverable
    /// INTID. **A no-op:** no fabric is wired in the skeleton (the `gicv3`
    /// arbitration model is M2's; delivery into a real guest is AA-6-gated),
    /// so the backend's inject seam is never touched — exactly the x86
    /// unwired-LAPIC path, whose state and `state_hash` this mirrors.
    pub(crate) fn service_pending_irqs_arm64(&mut self) -> Result<(), VmmError> {
        Ok(())
    }

    /// Complete delivery of every identity the backend accepted during the
    /// last entry. With no fabric wired there is no pending→in-service
    /// transition to model; the accepted queue is still drained so a
    /// mock-injected identity can never sit stale across entries.
    pub(crate) fn complete_irq_delivery_arm64(&mut self) {
        while self.backend.take_accepted_interrupt().is_some() {}
    }

    /// Whether a deliverable interrupt is already pending in the fabric.
    /// No fabric ⇒ never.
    pub(crate) fn pending_deliverable_interrupt_arm64(&mut self) -> Result<bool, VmmError> {
        Ok(false)
    }

    /// The next armed fabric-timer deadline. No fabric ⇒ none (the generic
    /// timer's deadline seam wires through the `gicv3` model, M2).
    pub(crate) fn next_timer_deadline_vns_arm64(&self) -> Option<u64> {
        None
    }

    /// [`Self::next_timer_deadline_vns_arm64`], filtered to deliverable fires.
    pub(crate) fn deliverable_timer_deadline_vns_arm64(&self) -> Option<u64> {
        None
    }

    /// Stage-time validation of a wire-format interrupt identity. With no
    /// fabric wired every identity is [`InterruptReject::NoFabric`]; once the
    /// GICv3 wires (M2) the identity space is the **implemented,
    /// distributor-bounded** GICv3 space — SGIs `0..16` deliverable (never
    /// x86's reserved rule), PPIs, SPIs to the configured limit.
    pub(crate) fn check_wire_interrupt_arm64(&self, vector: u32) -> Result<(), InterruptReject> {
        let _ = vector;
        Err(InterruptReject::NoFabric)
    }

    /// Raise the wire-format interrupt into the fabric. **Fails loud:** no
    /// delivery fabric is wired in the skeleton (M2 wires the pure arbitration
    /// model; delivery into a real guest is AA-6's vGICv3-round-trip verdict).
    pub(crate) fn inject_host_interrupt_arm64(&mut self, vector: u32) -> Result<(), VmmError> {
        Err(VmmError::ContractViolation(format!(
            "InjectInterrupt INTID {vector:#x} but no arm64 delivery fabric is wired — the \
             GICv3 arbitration model is unwired in the skeleton and guest delivery is \
             AA-6-gated (the in-kernel vGICv3 round-trip verdict)"
        )))
    }

    /// Whether a genuine guest interrupt is pending delivery but not yet
    /// accepted. No fabric ⇒ never.
    pub(crate) fn has_pending_guest_interrupt_arm64(&mut self) -> Result<bool, VmmError> {
        Ok(false)
    }

    /// Build the canonical [`Arm64VmState`] from `vcpu` + the current live
    /// machine (the memory-less half of a snapshot): the arm64 record set, the
    /// V-time block + entropy stream, and the vmm-core-owned device blob
    /// (PL011 residuals, the report stream, the guest clock offset). The
    /// `contract_hash` is stamped so a restore can reject a blob taken under a
    /// different policy skeleton. Infallible and byte-deterministic — the
    /// V-time block anchors to the deterministic `last_intercept_work`,
    /// exactly like the x86 builder.
    pub(crate) fn build_vm_state_arm64(&self, vcpu: &Arm64VcpuState) -> Arm64VmState {
        let mut s = Arm64VmState::default();
        records::fill_vcpu_state(&mut s, vcpu);
        let clock_offset = match &self.vtime {
            Some(vt) => {
                s.vtime = vm_state::VtimeState {
                    ratio_num: vt.cfg.ratio_num,
                    // `VtimeWiring::new` enforces `ratio_den == 1`; carry it so
                    // the blob is encodable.
                    ratio_den: 1,
                    guest_hz: vt.cfg.guest_hz,
                    guest_base: vt.cfg.guest_base,
                    snapshot_vns: vt.clock.snapshot_vns(vt.last_intercept_work),
                };
                s.hypercall = vt.entropy.save_state();
                vt.guest_clock_offset
            }
            None => {
                // Unwired: a sentinel encodable V-time block, no entropy.
                s.vtime.ratio_den = 1;
                0
            }
        };
        let dev = Arm64DeviceState {
            clock_offset,
            report_stream: self.report_stream.clone(),
            uart_capture: self.devices.uart.capture().to_vec(),
            uart_regs: *self.devices.uart.shadow_regs(),
        };
        s.devices = records::encode_device_blob(&dev);
        s.contract_hash = contract::contract_hash();
        s
    }

    /// The arm64 half of a snapshot restore, **validating without mutating**:
    /// the contract hash, the device blob, and the channel composition. Yields
    /// the decoded vCPU record set, the guest clock-offset register the engine
    /// re-applies with its V-time commit, and the prepared device state for
    /// [`Self::commit_restore_arm64`].
    pub(crate) fn validate_restore_arm64(
        &self,
        s: &Arm64VmState,
    ) -> Result<(Arm64VcpuState, u64, Arm64RestorePrep), VmmError> {
        // A blob taken under a different policy skeleton would silently
        // diverge on restore (the x86 `contract_hash` discipline).
        if s.contract_hash != contract::contract_hash() {
            return Err(VmmError::Snapshot(SnapshotError::ContractMismatch));
        }
        // Decode the vmm-core device blob (total, never panics).
        let dev = records::decode_device_blob(&s.devices.0)?;
        // The arm64 skeleton blob carries **no pvclock channel record** (the
        // arm64 clock-page protocol is `hm-rk5`'s; this skeleton only reserves
        // the seam). Validate that symmetrically against this VM's
        // composition: a pvclock-wired restore target fails loud rather than
        // silently forking the sealed timeline.
        self.pvclock_validate_restore(None)?;
        let vcpu = records::vcpu_state_from(s);
        let clock_offset = dev.clock_offset;
        Ok((vcpu, clock_offset, Arm64RestorePrep { dev }))
    }

    /// The arm64 half of the restore **commit** (all infallible): install the
    /// PL011 residual state and the restored guest-observable report stream.
    pub(crate) fn commit_restore_arm64(&mut self, prep: Arm64RestorePrep) {
        let Arm64RestorePrep { dev } = prep;
        self.devices.uart.restore(dev.uart_capture, dev.uart_regs);
        self.pvclock_commit_restore(None);
        self.report_stream = dev.report_stream;
    }
}

/// The arm64 half of a validated-but-uncommitted restore
/// ([`Vendor::validate_restore`](crate::vendor::Vendor::validate_restore) →
/// [`Vendor::commit_restore`](crate::vendor::Vendor::commit_restore)): the
/// decoded device blob.
pub struct Arm64RestorePrep {
    dev: Arm64DeviceState,
}

/// Deterministic, fixed-layout encoding of an [`Arm64VcpuState`] for the
/// engine's `VCPU` hash chunk (little-endian, declaration order; no map
/// iteration, no float). Canonicalizes exactly what the snapshot records
/// canonicalize, so a restored VM hashes like a never-restored one.
pub(crate) fn encode_vcpu_state(s: &Arm64VcpuState) -> Vec<u8> {
    let mut v = Vec::new();
    for x in s.core.x {
        v.extend_from_slice(&x.to_le_bytes());
    }
    for x in [
        s.core.sp,
        s.core.pc,
        s.core.pstate,
        s.core.sp_el1,
        s.core.elr_el1,
        s.core.spsr_el1,
    ] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    for x in [
        s.sysregs.sctlr_el1,
        s.sysregs.ttbr0_el1,
        s.sysregs.ttbr1_el1,
        s.sysregs.tcr_el1,
        s.sysregs.mair_el1,
        s.sysregs.vbar_el1,
        s.sysregs.cpacr_el1,
        s.sysregs.esr_el1,
        s.sysregs.far_el1,
        s.sysregs.tpidr_el0,
        s.sysregs.tpidr_el1,
        s.sysregs.cntkctl_el1,
    ] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.push(match s.mp_state {
        vmm_backend::MpState::Runnable => 0,
        vmm_backend::MpState::Halted => 1,
    });
    v
}

/// The arm64 register-file breakdown for the **diagnostic**
/// [`Vmm::state_components`] (never part of `state_hash`), so a determinism
/// bisector can localize which register file diverged. Labels are stable and
/// in a fixed order (the arm64 vendor's own label set — the x86 labels
/// `regs`/`desc-tables`/… are that vendor's and stay untouched).
pub(crate) fn vcpu_components(s: &Arm64VcpuState, out: &mut Vec<(&'static str, [u8; 32])>) {
    fn dig(bytes: &[u8]) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(bytes);
        h.finalize().into()
    }

    let mut core = Vec::new();
    for x in s.core.x {
        core.extend_from_slice(&x.to_le_bytes());
    }
    for x in [
        s.core.sp,
        s.core.pc,
        s.core.pstate,
        s.core.sp_el1,
        s.core.elr_el1,
        s.core.spsr_el1,
    ] {
        core.extend_from_slice(&x.to_le_bytes());
    }
    out.push(("core-regs", dig(&core)));

    let mut sys = Vec::new();
    for x in [
        s.sysregs.sctlr_el1,
        s.sysregs.ttbr0_el1,
        s.sysregs.ttbr1_el1,
        s.sysregs.tcr_el1,
        s.sysregs.mair_el1,
        s.sysregs.vbar_el1,
        s.sysregs.cpacr_el1,
        s.sysregs.esr_el1,
        s.sysregs.far_el1,
        s.sysregs.tpidr_el0,
        s.sysregs.tpidr_el1,
        s.sysregs.cntkctl_el1,
    ] {
        sys.extend_from_slice(&x.to_le_bytes());
    }
    out.push(("sysregs", dig(&sys)));

    let mp = match s.mp_state {
        vmm_backend::MpState::Runnable => 0u8,
        vmm_backend::MpState::Halted => 1,
    };
    out.push(("mp_state", dig(&[mp])));
}
