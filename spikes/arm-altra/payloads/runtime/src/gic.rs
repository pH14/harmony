// SPDX-License-Identifier: AGPL-3.0-or-later
//! Minimal GICv3 bring-up: distributor, redistributor, and the CPU interface's
//! system-register face.
//!
//! Only the virtual-timer PPI is enabled — it is the wake source for the
//! `wfi-idle` payload and the only interrupt any payload takes. The GIC is
//! initialized for *every* payload, not just that one, so that the pre-window
//! code is identical across the payload set: the per-class constant offset
//! stage AA-1 measures is only comparable across classes if the code before the
//! window is the same code.
//!
//! The GPAs match QEMU `virt`'s GICv3 map. The KVM harness places the in-kernel
//! vGICv3 at the same addresses, so the payloads are byte-identical in both
//! environments. Whether that in-kernel vGIC can be state-saved and restored
//! bit-identically is stage AA-6's measured question, not an assumption here.

/// GICv3 distributor.
const GICD: u64 = 0x0800_0000;
/// GICv3 redistributor, RD_base frame for CPU 0.
const GICR: u64 = 0x080A_0000;
/// The redistributor's SGI/PPI frame sits 64 KiB above RD_base.
const GICR_SGI: u64 = GICR + 0x1_0000;

/// Distributor control.
const GICD_CTLR: u64 = 0x0000;
/// `GICD_CTLR.ARE_NS` — affinity routing, which GICv3 requires.
const CTLR_ARE_NS: u32 = 1 << 4;
/// `GICD_CTLR.EnableGrp1NS`.
const CTLR_GRP1NS: u32 = 1 << 1;

/// Redistributor wake control.
const GICR_WAKER: u64 = 0x0014;
/// `GICR_WAKER.ProcessorSleep`.
const WAKER_PS: u32 = 1 << 1;
/// `GICR_WAKER.ChildrenAsleep`.
const WAKER_CA: u32 = 1 << 2;

/// Interrupt group (SGI/PPI frame).
const GICR_IGROUPR0: u64 = 0x0080;
/// Set-enable (SGI/PPI frame).
const GICR_ISENABLER0: u64 = 0x0100;
/// Priority bytes (SGI/PPI frame).
const GICR_IPRIORITYR: u64 = 0x0400;

/// INTID of the EL1 virtual timer's private peripheral interrupt. Enabled but
/// unused by the payload set — see [`SGI_WAKE`] for why the idle payload does not
/// wake on a timer. Left enabled so AA-5/AA-6 have the timer path available
/// without touching the runtime.
pub const PPI_VIRT_TIMER: u32 = 27;

/// INTID of the software-generated interrupt the `wfi-idle` payload sends to
/// itself. An SGI is the wake source precisely because it makes the interrupt
/// pending *before* the `WFI`, so the payload never has to spin waiting for one —
/// and a spin's back-edge would be a wall-clock-dependent taken branch inside a
/// counting window, which would defeat the oracle. See `oracles/src/asm/wfi_idle.s`.
pub const SGI_WAKE: u32 = 1;

/// # Safety
/// `addr` must be a GIC MMIO register. The GIC window is mapped Device-nGnRnE by
/// the boot shim.
unsafe fn w32(addr: u64, value: u32) {
    // SAFETY: caller guarantees `addr` is a GIC register in the mapped window.
    unsafe { core::ptr::write_volatile(addr as *mut u32, value) }
}

/// # Safety
/// As [`w32`].
unsafe fn r32(addr: u64) -> u32 {
    // SAFETY: as above.
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

/// Bring up the GIC far enough to take the virtual-timer PPI at EL1.
pub fn init() {
    // SAFETY: every address below is a GICv3 register in the Device-mapped
    // window; the sequence is the architected bring-up order (affinity routing
    // before group enable; redistributor woken before its frames are touched;
    // ICC_SRE_EL1.SRE set before any other ICC_* system register is accessed).
    unsafe {
        // Distributor: affinity routing first, then non-secure group 1.
        w32(GICD + GICD_CTLR, CTLR_ARE_NS);
        w32(GICD + GICD_CTLR, CTLR_ARE_NS | CTLR_GRP1NS);

        // Redistributor: clear ProcessorSleep and wait for the children to wake.
        let waker = r32(GICR + GICR_WAKER) & !WAKER_PS;
        w32(GICR + GICR_WAKER, waker);
        while r32(GICR + GICR_WAKER) & WAKER_CA != 0 {}

        // All SGIs/PPIs to group 1. The two interrupts the payloads can take get a
        // priority that passes the mask set below, and are enabled.
        w32(GICR_SGI + GICR_IGROUPR0, 0xFFFF_FFFF);
        for intid in [SGI_WAKE, PPI_VIRT_TIMER] {
            core::ptr::write_volatile(
                (GICR_SGI + GICR_IPRIORITYR + u64::from(intid)) as *mut u8,
                0xA0,
            );
        }
        w32(
            GICR_SGI + GICR_ISENABLER0,
            (1 << SGI_WAKE) | (1 << PPI_VIRT_TIMER),
        );

        // CPU interface. SRE must be set (and an ISB taken) before any other
        // ICC_* access.
        let sre: u64;
        core::arch::asm!(
            "mrs {sre}, icc_sre_el1",
            sre = out(reg) sre,
            options(nomem, nostack, preserves_flags),
        );
        core::arch::asm!(
            "msr icc_sre_el1, {sre}",
            "isb",
            sre = in(reg) sre | 1,
            options(nomem, nostack, preserves_flags),
        );
        core::arch::asm!(
            "msr icc_pmr_el1, {pmr}",       // priority mask: allow 0x00..0xEF
            "msr icc_bpr1_el1, {bpr}",      // no preemption grouping
            "msr icc_igrpen1_el1, {en}",    // enable group 1
            "isb",
            pmr = in(reg) 0xF0_u64,
            bpr = in(reg) 0_u64,
            en = in(reg) 1_u64,
            options(nomem, nostack, preserves_flags),
        );
    }
}
