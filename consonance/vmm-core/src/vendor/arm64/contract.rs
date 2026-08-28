// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arm64 CPU-contract policy shared by the HVF and KVM compositions.
//!
//! The x86 contract (`docs/cpu-msr-contract.toml`, the `vendor::x86::contract`
//! module) is the rigor template, not the content. M5 measured both live hosts,
//! selected a conservative common feature surface, and validated every row
//! through KVM's config-time writable-ID-register API. Values KVM does not
//! permit userspace to reduce (ASID/VMID width and EL2-only fields) retain the
//! KVM value; they are harmless to the EL1 payload and are installed into HVF
//! as part of the same synthetic identity.
//!
//! The runtime trap table remains empty because stock KVM has no userspace
//! sysreg-exit surface; the cooperative-image and audit closures remain the
//! enforcement posture for those instructions.

use sha2::{Digest, Sha256};
use vmm_backend::{Arm64Policy, IdRegModel, SysregTrapPolicy};

use crate::virtual_time::VirtualTimeTiming;

/// Portable duration of interrupt-controller transactions.
///
/// Stock KVM consumes GIC distributor, redistributor, and CPU-interface
/// accesses inside the in-kernel vGIC, while HVF surfaces them to userspace.
/// They therefore remain raw diagnostics but contribute no portable V-time.
pub const INTERRUPT_CONTROLLER_EXIT_VNS: u64 = 0;
/// Assigned duration of one PL011 access.
pub const SERIAL_EXIT_VNS: u64 = 2_000;
/// Assigned duration of one pvclock/clockevent MMIO access.
pub const PARAVIRTUAL_EXIT_VNS: u64 = 1_000;
/// Assigned duration of the kernel's deterministic execution tick.
///
/// The guest emits one tick on every syscall entry and context switch. This
/// quantum must remain strictly below Linux's 100 Hz clockevent period: a
/// timer interrupt can itself cause a context switch, and advancing by a full
/// period there would immediately mature its successor and create a
/// self-sustaining interrupt loop.
pub(crate) const EXECUTION_TICK_VNS: u64 = 1_000_000;
pub(crate) const LINUX_CLOCKEVENT_PERIOD_VNS: u64 = 10_000_000;
const _: () = assert!(EXECUTION_TICK_VNS < LINUX_CLOCKEVENT_PERIOD_VNS);
/// Assigned duration of a trapped counter-shaped time read.
pub const TRAPPED_TIME_READ_VNS: u64 = 1;
/// Assigned duration of a deterministic architectural-control trap that is
/// neither a device access nor a time read (for example Linux clearing the
/// OS debug lock at boot).
pub const ARCH_CONTROL_EXIT_VNS: u64 = 1_000;

/// The normative arm64 virtual_time timing row set. Production composition
/// never uses `VirtualTimeTiming::default()`'s M0 placeholders.
pub fn virtual_time_timing() -> VirtualTimeTiming {
    VirtualTimeTiming {
        interrupt_controller_mmio_vns: INTERRUPT_CONTROLLER_EXIT_VNS,
        serial_mmio_vns: SERIAL_EXIT_VNS,
        paravirtual_device_mmio_vns: PARAVIRTUAL_EXIT_VNS,
        trapped_time_read_vns: TRAPPED_TIME_READ_VNS,
        architectural_control_vns: ARCH_CONTROL_EXIT_VNS,
    }
}

/// Canonical packed system-register encodings and M5 cross-host baseline.
///
/// The values were read independently by `hvf_probe` and
/// `arm64_kvm_id_probe`. The latter also writes and reads back each selected
/// value before first entry, proving that stock KVM accepts the complete set.
pub const IDENTITY_BASELINE: [(u32, u64); 21] = [
    (0xc000, 0x0000_0000_410f_d811), // MIDR_EL1
    (0xc005, 0x0000_0000_8000_0000), // MPIDR_EL1
    (0xc020, 0x1101_0000_1111_0011), // ID_AA64PFR0_EL1
    (0xc021, 0x0000_0000_0000_0000), // ID_AA64PFR1_EL1
    (0xc022, 0x0000_0000_0000_0000), // ID_AA64PFR2_EL1
    (0xc024, 0x0000_0000_0000_0000), // ID_AA64ZFR0_EL1
    (0xc025, 0x0000_0000_0000_0000), // ID_AA64SMFR0_EL1
    (0xc027, 0x0000_0000_0000_0000), // ID_AA64FPFR0_EL1
    (0xc028, 0x0000_00f0_1030_5006), // ID_AA64DFR0_EL1
    (0xc029, 0x0000_0000_0000_0000), // ID_AA64DFR1_EL1
    (0xc02a, 0x0000_0000_0000_0000), // ID_AA64DFR2_EL1
    (0xc030, 0x0221_1001_1021_2120), // ID_AA64ISAR0_EL1
    (0xc031, 0x0000_0111_0021_1002), // ID_AA64ISAR1_EL1
    (0xc032, 0x0000_0000_0000_0000), // ID_AA64ISAR2_EL1
    (0xc033, 0x0000_0000_0000_0000), // ID_AA64ISAR3_EL1
    (0xc038, 0x0000_0111_0f10_0022), // ID_AA64MMFR0_EL1
    (0xc039, 0x0000_0000_1121_2120), // ID_AA64MMFR1_EL1
    (0xc03a, 0x1201_0111_0000_1011), // ID_AA64MMFR2_EL1
    (0xc03b, 0x0000_0000_0000_0000), // ID_AA64MMFR3_EL1
    (0xc03c, 0x0000_0000_0000_0000), // ID_AA64MMFR4_EL1
    (0xd801, 0x0000_0000_8444_c004), // CTR_EL0
];

/// Guest-visible identity that neither substrate exposes as writable state.
///
/// Both live instruction probes read this exact value. It is bound into the
/// contract hash even though it cannot be installed through either substrate's
/// configuration API; a host with a different value is not M5-qualified.
pub const READ_ONLY_IDENTITY_BASELINE: [(u32, u64); 1] = [
    (0xd807, 0x0000_0000_0000_0004), // DCZID_EL0
];

/// The installable arm64 policy: the frozen cross-host identity and the empty
/// stock-substrate trap table.
pub fn policy() -> Arm64Policy {
    Arm64Policy {
        id_regs: IdRegModel {
            regs: IDENTITY_BASELINE.into_iter().collect(),
        },
        sysreg_traps: SysregTrapPolicy::default(),
    }
}

/// SHA-256 over the canonical encoding of the installed policy — the arm64
/// snapshot's `contract_hash` anchor. Two builds whose policy rows differ
/// stamp different hashes, so a snapshot taken under one contract baseline is
/// refused by a VMM enforcing another (the same anti-drift role as the x86
/// `contract_hash`, INTEGRATION.md §4). The domain-separation prefix names this
/// baseline explicitly so it cannot collide with the earlier empty skeleton.
pub fn contract_hash() -> [u8; 32] {
    let p = policy();
    let mut h = Sha256::new();
    h.update(b"harmony-arm64-cross-host-baseline-v2\0");
    // Canonical encoding: sorted (BTreeMap/BTreeSet) rows, little-endian
    // fixed-width fields, length-prefixed sections — deterministic (rule #4).
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
        assert_eq!(contract_hash(), contract_hash());
        // One changed row must hash differently — the anti-drift property the
        // snapshot check relies on.
        let mut p = policy();
        p.id_regs.regs.insert(0xc020, 0x1122);
        let mut h = Sha256::new();
        h.update(b"harmony-arm64-cross-host-baseline-v2\0");
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
        h.update(0u64.to_le_bytes());
        let with_row: [u8; 32] = h.finalize().into();
        assert_ne!(contract_hash(), with_row);
    }

    #[test]
    fn production_virtual_time_timing_is_explicit_not_the_m0_default() {
        let timing = virtual_time_timing();
        assert_ne!(timing, VirtualTimeTiming::default());
        assert_eq!(timing.interrupt_controller_mmio_vns, 0);
        assert_eq!(timing.serial_mmio_vns, 2_000);
        assert_eq!(timing.paravirtual_device_mmio_vns, 1_000);
        assert_eq!(timing.trapped_time_read_vns, 1);
        assert_eq!(timing.architectural_control_vns, 1_000);
    }
}
