// SPDX-License-Identifier: AGPL-3.0-or-later

use sha2::{Digest, Sha256};
use vmm_backend::{Arm64Policy, IdRegModel, SysregTrapPolicy};

use crate::virtual_time::VirtualTimeTiming;

pub const INTERRUPT_CONTROLLER_EXIT_VNS: u64 = 10_000;
pub const SERIAL_EXIT_VNS: u64 = 10_000;
pub const PARAVIRTUAL_EXIT_VNS: u64 = 10_000;
pub(crate) const EXECUTION_TICK_VNS: u64 = 100_000;
pub(crate) const LINUX_CLOCKEVENT_PERIOD_VNS: u64 = 10_000_000;
const _: () = assert!(EXECUTION_TICK_VNS < LINUX_CLOCKEVENT_PERIOD_VNS);
pub const TRAPPED_TIME_READ_VNS: u64 = 1;
pub const ARCH_CONTROL_EXIT_VNS: u64 = 10_000;

pub fn virtual_time_timing() -> VirtualTimeTiming {
    VirtualTimeTiming {
        interrupt_controller_mmio_vns: INTERRUPT_CONTROLLER_EXIT_VNS,
        serial_mmio_vns: SERIAL_EXIT_VNS,
        paravirtual_device_mmio_vns: PARAVIRTUAL_EXIT_VNS,
        trapped_time_read_vns: TRAPPED_TIME_READ_VNS,
        architectural_control_vns: ARCH_CONTROL_EXIT_VNS,
        execution_tick_vns: EXECUTION_TICK_VNS,
    }
}

pub const IDENTITY_BASELINE: [(u32, u64); 21] = [
    (0xc000, 0x0000_0000_410f_d811),
    (0xc005, 0x0000_0000_8000_0000),
    (0xc020, 0x1101_0000_1111_0011),
    (0xc021, 0x0000_0000_0000_0000),
    (0xc022, 0x0000_0000_0000_0000),
    (0xc024, 0x0000_0000_0000_0000),
    (0xc025, 0x0000_0000_0000_0000),
    (0xc027, 0x0000_0000_0000_0000),
    (0xc028, 0x0000_00f0_1030_5006),
    (0xc029, 0x0000_0000_0000_0000),
    (0xc02a, 0x0000_0000_0000_0000),
    (0xc030, 0x0221_1001_1021_2120),
    (0xc031, 0x0000_0111_0021_1002),
    (0xc032, 0x0000_0000_0000_0000),
    (0xc033, 0x0000_0000_0000_0000),
    (0xc038, 0x0000_0111_0f10_0022),
    (0xc039, 0x0000_0000_1121_2120),
    (0xc03a, 0x1201_0111_0000_1011),
    (0xc03b, 0x0000_0000_0000_0000),
    (0xc03c, 0x0000_0000_0000_0000),
    (0xd801, 0x0000_0000_8444_c004),
];

pub const READ_ONLY_IDENTITY_BASELINE: [(u32, u64); 1] = [(0xd807, 0x0000_0000_0000_0004)];

pub fn policy() -> Arm64Policy {
    Arm64Policy {
        id_regs: IdRegModel {
            regs: IDENTITY_BASELINE.into_iter().collect(),
        },
        sysreg_traps: SysregTrapPolicy::default(),
    }
}

pub fn contract_hash() -> [u8; 32] {
    let p = policy();
    let mut h = Sha256::new();
    h.update(b"harmony-arm64-cross-host-baseline-v3\0");
    h.update((p.id_regs.regs.len() as u64).to_le_bytes());
    for (enc, val) in &p.id_regs.regs {
        h.update(enc.to_le_bytes());
        h.update(val.to_le_bytes());
    }
    h.update((READ_ONLY_IDENTITY_BASELINE.len() as u64).to_le_bytes());
    for (encoding, value) in READ_ONLY_IDENTITY_BASELINE {
        h.update(encoding.to_le_bytes());
        h.update(value.to_le_bytes());
    }
    h.update((p.sysreg_traps.trapped.len() as u64).to_le_bytes());
    for enc in &p.sysreg_traps.trapped {
        h.update(enc.to_le_bytes());
    }
    let t = virtual_time_timing();
    for vns in [
        t.interrupt_controller_mmio_vns,
        t.serial_mmio_vns,
        t.paravirtual_device_mmio_vns,
        t.trapped_time_read_vns,
        t.architectural_control_vns,
        t.execution_tick_vns,
        LINUX_CLOCKEVENT_PERIOD_VNS,
    ] {
        h.update(vns.to_le_bytes());
    }
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_contains_the_sorted_cross_host_identity_and_empty_trap_table() {
        let p = policy();
        assert_eq!(p.id_regs.regs.len(), IDENTITY_BASELINE.len());
        assert_eq!(
            p.id_regs
                .regs
                .iter()
                .map(|(&encoding, &value)| (encoding, value))
                .collect::<Vec<_>>(),
            IDENTITY_BASELINE
        );
        assert_eq!(READ_ONLY_IDENTITY_BASELINE, [(0xd807, 4)]);
        assert!(p.sysreg_traps.trapped.is_empty());
    }

    #[test]
    fn contract_hash_is_deterministic_and_row_sensitive() {
        let frozen = contract_hash();
        assert_eq!(frozen, contract_hash());
        assert_ne!(frozen, [0; 32]);
        assert_ne!(frozen, [1; 32]);
        let mut p = policy();
        p.id_regs.regs.insert(0xc020, 0x1122);
        let with_row = recompute(&p, virtual_time_timing());
        assert_ne!(contract_hash(), with_row);
        let mut timing = virtual_time_timing();
        timing.serial_mmio_vns += 1;
        let with_timing = recompute(&policy(), timing);
        assert_ne!(contract_hash(), with_timing);
    }

    fn recompute(p: &Arm64Policy, t: VirtualTimeTiming) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"harmony-arm64-cross-host-baseline-v3\0");
        h.update((p.id_regs.regs.len() as u64).to_le_bytes());
        for (encoding, value) in &p.id_regs.regs {
            h.update(encoding.to_le_bytes());
            h.update(value.to_le_bytes());
        }
        h.update((READ_ONLY_IDENTITY_BASELINE.len() as u64).to_le_bytes());
        for (encoding, value) in READ_ONLY_IDENTITY_BASELINE {
            h.update(encoding.to_le_bytes());
            h.update(value.to_le_bytes());
        }
        h.update((p.sysreg_traps.trapped.len() as u64).to_le_bytes());
        for enc in &p.sysreg_traps.trapped {
            h.update(enc.to_le_bytes());
        }
        for vns in [
            t.interrupt_controller_mmio_vns,
            t.serial_mmio_vns,
            t.paravirtual_device_mmio_vns,
            t.trapped_time_read_vns,
            t.architectural_control_vns,
            t.execution_tick_vns,
            LINUX_CLOCKEVENT_PERIOD_VNS,
        ] {
            h.update(vns.to_le_bytes());
        }
        h.finalize().into()
    }

    #[test]
    fn production_virtual_time_timing_is_explicit_not_the_m0_default() {
        let timing = virtual_time_timing();
        assert_ne!(timing, VirtualTimeTiming::default());
        assert_eq!(timing.interrupt_controller_mmio_vns, 10_000);
        assert_eq!(timing.serial_mmio_vns, 10_000);
        assert_eq!(timing.paravirtual_device_mmio_vns, 10_000);
        assert_eq!(timing.trapped_time_read_vns, 1);
        assert_eq!(timing.architectural_control_vns, 10_000);
        assert_eq!(timing.execution_tick_vns, 100_000);
    }

    #[test]
    fn both_architectures_share_one_timing_row_set() {
        assert_eq!(
            virtual_time_timing(),
            crate::vendor::x86::contract::virtual_time_timing()
        );
        assert_eq!(
            LINUX_CLOCKEVENT_PERIOD_VNS,
            crate::vendor::x86::contract::clockevent_period_vns()
        );
    }
}
