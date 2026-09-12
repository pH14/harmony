// SPDX-License-Identifier: AGPL-3.0-or-later

use vmm_backend::{Arm64VcpuState, MpState};

pub const PSTATE_EL1H_DAIF: u64 = 0x3c5;

pub fn boot_entry(entry_gpa: u64, dtb_gpa: u64) -> Arm64VcpuState {
    let mut s = Arm64VcpuState::default();
    s.core.pc = entry_gpa;
    s.core.x[0] = dtb_gpa;
    s.core.pstate = PSTATE_EL1H_DAIF;
    s.mp_state = MpState::Runnable;
    s
}

pub fn apply_entry(state: &mut Arm64VcpuState, entry: &Arm64VcpuState) {
    state.core = entry.core;
    state.mp_state = entry.mp_state;
    let sctlr_el1 = state.sysregs.sctlr_el1;
    state.sysregs = entry.sysregs;
    state.sysregs.sctlr_el1 = sctlr_el1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_entry_matches_the_arm64_protocol() {
        let s = boot_entry(0x4020_0000, 0x4f00_0000);
        assert_eq!(s.core.pc, 0x4020_0000);
        assert_eq!(s.core.x[0], 0x4f00_0000);
        assert_eq!(s.core.x[1], 0);
        assert_eq!(s.core.x[2], 0);
        assert_eq!(s.core.x[3], 0);
        assert_eq!(s.core.pstate, 0x3c5);
        assert_eq!(s.mp_state, MpState::Runnable);
    }

    #[test]
    fn apply_entry_overlays_core_regs_and_canonicalizes_unknown_sysregs() {
        let mut template = Arm64VcpuState::default();
        template.sysregs.sctlr_el1 = 0x30d0_0800;
        template.sysregs.mair_el1 = 0x00ff_0044;
        template.sysregs.esr_el1 = 0x1de7_ec7e_dbad_c0de;
        let entry = boot_entry(0x4020_0000, 0x4f00_0000);
        apply_entry(&mut template, &entry);
        assert_eq!(template.core.pc, 0x4020_0000);
        assert_eq!(template.core.x[0], 0x4f00_0000);
        assert_eq!(template.core.pstate, 0x3c5);
        assert_eq!(template.sysregs.sctlr_el1, 0x30d0_0800);
        assert_eq!(template.sysregs.mair_el1, 0);
        assert_eq!(template.sysregs.esr_el1, 0);
    }
}
