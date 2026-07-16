// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arm64 boot entry state (`tasks/112` M3) — the arm64 analogue of the x86
//! `entry`, but tiny: the Linux/arm64 boot protocol
//! (`Documentation/arm64/booting.rst`) is "enter at the image's first
//! instruction, at EL1 (under KVM), with `x0` = the DTB physical address and
//! `x1..x3` = 0". No GDT/IDT/page-table/segment apparatus.
//!
//! The returned [`Arm64VcpuState`] is overlaid onto a backend `save()` template
//! by the composition root (the get→modify→set pattern), exactly as the x86
//! entry state is — a live `restore` validates register shapes a pure builder
//! cannot know.

use vmm_backend::{Arm64VcpuState, MpState};

/// `PSTATE`/`SPSR` value for **EL1h** (EL1 using `SP_EL1`) with `DAIF` masked —
/// the reset processor state the arm64 boot protocol enters with.
///
/// Bit layout: `M[3:0] = 0b0101` (EL1h) with `M[4] = 0` (AArch64), and the
/// `DAIF` mask bits `D`(9) `A`(8) `I`(7) `F`(6) all set = `0x3c0`. Together
/// `0x3c0 | 0x5 = 0x3c5`. Written as the composed literal (not `0x3c0 | 5`) so
/// the value the guest actually sees is unmistakable in one place.
pub const PSTATE_EL1H_DAIF: u64 = 0x3c5;

/// Build the boot entry state: `PC` at `entry_gpa`, `x0` = `dtb_gpa`,
/// `x1..x3 = 0`, `PSTATE` = [`PSTATE_EL1H_DAIF`], and `MpState::Runnable`.
/// Every other register is left zero (the boot protocol requires nothing of
/// them, and a live template supplies whatever the backend's `restore`
/// validates).
pub fn boot_entry(entry_gpa: u64, dtb_gpa: u64) -> Arm64VcpuState {
    let mut s = Arm64VcpuState::default();
    s.core.pc = entry_gpa;
    s.core.x[0] = dtb_gpa;
    // x1, x2, x3 are already zero (Default) — the boot protocol's reserved regs.
    s.core.pstate = PSTATE_EL1H_DAIF;
    s.mp_state = MpState::Runnable;
    s
}

/// Overlay the boot entry registers onto a backend `save()` template, keeping
/// the template's valid EL1 sysreg shape (which a live `restore` validates).
/// The arm64 analogue of the x86 `apply_entry`.
pub fn apply_entry(state: &mut Arm64VcpuState, entry: &Arm64VcpuState) {
    state.core = entry.core;
    state.mp_state = entry.mp_state;
    // The EL1 sysreg file is left as the template's (the guest sets up its own
    // MMU/vectors from reset; the skeleton carries the reset values the backend
    // reports).
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_entry_matches_the_arm64_protocol() {
        let s = boot_entry(0x4020_0000, 0x4f00_0000);
        assert_eq!(s.core.pc, 0x4020_0000);
        assert_eq!(s.core.x[0], 0x4f00_0000); // x0 = DTB GPA
        assert_eq!(s.core.x[1], 0);
        assert_eq!(s.core.x[2], 0);
        assert_eq!(s.core.x[3], 0);
        assert_eq!(s.core.pstate, 0x3c5); // EL1h + DAIF masked
        assert_eq!(s.mp_state, MpState::Runnable);
    }

    #[test]
    fn apply_entry_overlays_core_regs_keeps_template_sysregs() {
        let mut template = Arm64VcpuState::default();
        template.sysregs.sctlr_el1 = 0x30d0_0800; // the backend's reset SCTLR
        template.sysregs.mair_el1 = 0x00ff_0044;
        let entry = boot_entry(0x4020_0000, 0x4f00_0000);
        apply_entry(&mut template, &entry);
        // Core registers came from the entry state...
        assert_eq!(template.core.pc, 0x4020_0000);
        assert_eq!(template.core.x[0], 0x4f00_0000);
        assert_eq!(template.core.pstate, 0x3c5);
        // ...but the template's EL1 sysreg shape survived.
        assert_eq!(template.sysregs.sctlr_el1, 0x30d0_0800);
        assert_eq!(template.sysregs.mair_el1, 0x00ff_0044);
    }
}
