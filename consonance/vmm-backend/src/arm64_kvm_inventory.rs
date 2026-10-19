// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use sha2::{Digest, Sha256};

const DEMUX: u64 = KVM_REG_ARM64 | KVM_REG_SIZE_U32 | (0x0011 << 16);
const PHYSICAL_COUNTER: u64 = sysreg_id(3, 3, 14, 0, 1);
const RESERVED_IDS: [u16; 14] = [
    0xc01b, 0xc01f, 0xc023, 0xc026, 0xc02b, 0xc02e, 0xc02f, 0xc034, 0xc035, 0xc036, 0xc037, 0xc03d,
    0xc03e, 0xc03f,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Disposition {
    Captured = 1,
    FixedZero,
    OsLock,
    Immutable,
    ExcludedAarch32,
    ExcludedPhysicalCounter,
    Firmware,
}

fn error(register: u64, reason: &'static str) -> BackendError {
    BackendError::Arm64RegisterInventory { register, reason }
}

fn sysreg(encoding: u16) -> u64 {
    KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM64_SYSREG | u64::from(encoding)
}

fn identity_register(id: u64) -> bool {
    id & !0xffff == KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM64_SYSREG
        && matches!(
            id as u16,
            0xc000 | 0xc005 | 0xc006 | 0xc008..=0xc03f | 0xc801 | 0xc807 | 0xd801
        )
}

pub(super) fn disposition(id: u64) -> Option<Disposition> {
    let class = id & !0xffff;
    let offset = id & 0xffff;
    if class == core_reg(0) {
        return if offset <= 72 && offset.is_multiple_of(2) {
            Some(Disposition::Captured)
        } else if (74..=80).contains(&offset) && offset.is_multiple_of(2) {
            Some(Disposition::ExcludedAarch32)
        } else {
            None
        };
    }
    if class == core_reg_sized(KVM_REG_SIZE_U128, 0) {
        return ((CORE_FP_BASE..=CORE_FP_BASE + 31 * 4).contains(&offset)
            && (offset - CORE_FP_BASE).is_multiple_of(4))
        .then_some(Disposition::Captured);
    }
    if class == core_reg_sized(KVM_REG_SIZE_U32, 0) {
        return [CORE_FPCR, CORE_FPSR]
            .contains(&offset)
            .then_some(Disposition::Captured);
    }
    if class == KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM64_SYSREG {
        let encoding = id as u16;
        if (0x8004..=0x807f).contains(&encoding) && matches!(encoding & 7, 4..=7)
            || matches!(
                encoding,
                0xc080
                    | 0xc100
                    | 0xc101
                    | 0xc102
                    | 0xc510
                    | 0xc600
                    | 0xc082
                    | 0xc290
                    | 0xc300
                    | 0xde82
                    | 0xde83
                    | 0xc684
                    | 0xc708
                    | 0xc681
                    | 0xc518
                    | 0xc3a0
                    | 0xd000
                    | 0xc288
                    | 0xc289
                    | 0x8010
                    | 0x8012
                    | 0xdf19
                    | 0xdf02
                    | 0xdf1a
            )
        {
            return Some(Disposition::Captured);
        }
        if matches!(encoding, 0xc081 | 0xc609 | 0xdf11 | 0xdf12) || RESERVED_IDS.contains(&encoding)
        {
            return Some(Disposition::FixedZero);
        }
        if id == OSLSR_EL1 {
            return Some(Disposition::OsLock);
        }
        if identity_register(id) {
            return Some(Disposition::Immutable);
        }
        if matches!(encoding, 0xa038 | 0xe180 | 0xe281 | 0xe298) {
            return Some(Disposition::ExcludedAarch32);
        }
        return (id == PHYSICAL_COUNTER).then_some(Disposition::ExcludedPhysicalCounter);
    }
    if class == DEMUX && offset < 14 {
        return Some(Disposition::Immutable);
    }
    if OPTIONAL_FIRMWARE_BITMAPS.contains(&id) {
        return Some(Disposition::FixedZero);
    }
    if id == KVM_REG_ARM_PSCI_VERSION || SMCCC_WORKAROUND_REGISTERS.contains(&id) {
        return Some(Disposition::Firmware);
    }
    None
}

fn baseline_registers() -> &'static [u64] {
    static REGISTER_IDS: std::sync::OnceLock<Vec<u64>> = std::sync::OnceLock::new();
    REGISTER_IDS
        .get_or_init(|| {
            let mut ids = (0..=80).step_by(2).map(core_reg).collect::<Vec<_>>();
            ids.extend(
                (0..32).map(|index| core_reg_sized(KVM_REG_SIZE_U128, CORE_FP_BASE + index * 4)),
            );
            ids.extend([CORE_FPCR, CORE_FPSR].map(|index| core_reg_sized(KVM_REG_SIZE_U32, index)));
            ids.extend(SYSREGS.iter().map(|&(id, _)| id));
            for index in 0..16 {
                ids.extend([dbgbvr(index), dbgbcr(index), dbgwvr(index), dbgwcr(index)]);
            }
            ids.extend([
                MDCCINT_EL1,
                MDSCR_EL1,
                OSLSR_EL1,
                ACTLR_EL1,
                DISR_EL1,
                CNTV_CTL_EL0,
                CNTV_CVAL_EL0,
                CNTVCT_EL0,
                CNTP_CTL_EL0,
                CNTP_CVAL_EL0,
                PHYSICAL_COUNTER,
            ]);
            ids.extend(
                [
                    0xa038, 0xe180, 0xe281, 0xe298, 0xc000, 0xc005, 0xc006, 0xc801, 0xc807, 0xd801,
                ]
                .map(sysreg),
            );
            ids.extend((0xc008..=0xc03f).map(sysreg));
            ids.extend((0..14).map(|selector| DEMUX | selector));
            ids.extend(OPTIONAL_FIRMWARE_BITMAPS);
            ids.push(KVM_REG_ARM_PSCI_VERSION);
            ids.extend(SMCCC_WORKAROUND_REGISTERS);
            ids.sort_unstable();
            ids
        })
        .as_slice()
}

#[cfg(any(test, feature = "mock"))]
pub(super) fn fake_registers() -> Vec<u64> {
    baseline_registers().to_vec()
}

pub(super) fn validate(mut ids: Vec<u64>) -> Result<Vec<(u64, Disposition)>> {
    ids.sort_unstable();
    if let Some(pair) = ids.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(error(pair[0], "duplicate inventory entry"));
    }
    let entries = ids
        .iter()
        .map(|&id| {
            disposition(id)
                .map(|kind| (id, kind))
                .ok_or_else(|| error(id, "no modeled disposition"))
        })
        .collect::<Result<Vec<_>>>()?;
    for &id in baseline_registers() {
        if disposition(id) == Some(Disposition::Captured) && ids.binary_search(&id).is_err() {
            return Err(error(id, "inventory omits captured state"));
        }
    }
    Ok(entries)
}

pub(super) fn validate_policy(
    policy: &crate::arch::arm64::Arm64Policy,
    entries: &[(u64, Disposition)],
) -> Result<()> {
    for (&encoding, &value) in &policy.id_regs.regs {
        let encoding = u16::try_from(encoding).map_err(|_| BackendError::InvalidState)?;
        let id = sysreg(encoding);
        if !identity_register(id)
            || RESERVED_IDS.contains(&encoding) && value != 0
            || entries.binary_search_by_key(&id, |&(id, _)| id).is_err()
        {
            return Err(error(id, "policy does not name a supported identity field"));
        }
    }
    Ok(())
}

pub(super) fn identity<K: Arm64Kvm + ?Sized>(
    kvm: &K,
    entries: &[(u64, Disposition)],
    enforce_fixed_state: bool,
) -> Result<[u8; 32]> {
    let mut hash = Sha256::new();
    hash.update(b"harmony-arm64-kvm-register-inventory-v1\0");
    for &(id, kind) in entries {
        hash.update(id.to_le_bytes());
        hash.update([kind as u8]);
        match kind {
            Disposition::FixedZero => {
                if !enforce_fixed_state
                    && [ACTLR_EL1, DISR_EL1, CNTP_CTL_EL0, CNTP_CVAL_EL0].contains(&id)
                {
                    continue;
                }
                if kvm.get_one_reg(id)? != 0 {
                    return Err(error(id, "register must remain fixed at zero"));
                }
            }
            Disposition::OsLock => {
                let value = kvm.get_one_reg(id)?;
                if enforce_fixed_state && value & OSLSR_OSLK != 0 {
                    return Err(error(id, "OS lock must remain clear"));
                }
                hash.update((value & !OSLSR_OSLK).to_le_bytes());
            }
            Disposition::Immutable | Disposition::Firmware => {
                let value = if id & !0xffff == DEMUX {
                    u64::from(kvm.get_one_reg32(id)?)
                } else {
                    kvm.get_one_reg(id)?
                };
                hash.update(value.to_le_bytes());
            }
            Disposition::Captured
            | Disposition::ExcludedAarch32
            | Disposition::ExcludedPhysicalCounter => {}
        }
    }
    Ok(hash.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake() -> FakeKvm {
        let mut kvm = FakeKvm::new();
        kvm.vcpu_init().unwrap();
        kvm
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
    #[ignore = "live ARM KVM inventory; registered in HOST_TESTS"]
    fn live_inventory_identity_and_fixed_state_are_enforced() {
        let mut backend = Arm64KvmBackend::new(crate::LiveKvm::new().unwrap());
        backend.set_policy(&crate::Arm64Policy::default()).unwrap();
        let entries = validate(backend.kvm.register_list().unwrap()).unwrap();
        eprintln!(
            "ARM_KVM_INVENTORY entries={} identity={:02x?}",
            entries.len(),
            backend.register_identity
        );
        for (id, kind) in &entries {
            eprintln!("ARM_KVM_REGISTER id={id:#018x} disposition={kind:?}");
        }
        assert_ne!(backend.register_identity, [0; 32]);
        let baseline = backend.save().unwrap();
        let revidr = sysreg(0xc006);
        let original = backend.kvm.get_one_reg(revidr).unwrap();
        backend.kvm.set_one_reg(revidr, original ^ 1).unwrap();
        assert!(backend.save().is_err());
        let pc = backend.kvm.get_one_reg(core_reg(CORE_PC)).unwrap();
        assert!(backend.restore(&baseline).is_err());
        assert_eq!(backend.kvm.get_one_reg(core_reg(CORE_PC)).unwrap(), pc);
        assert_eq!(backend.kvm.get_one_reg(revidr).unwrap(), original ^ 1);
        backend.kvm.set_one_reg(revidr, original).unwrap();
        backend.restore(&baseline).unwrap();
        assert_eq!(backend.save().unwrap().core.pc, baseline.core.pc);
        backend.kvm.set_one_reg(ACTLR_EL1, 2).unwrap();
        assert!(backend.save().is_err());
        backend.restore(&baseline).unwrap();
        assert_eq!(backend.kvm.get_one_reg(ACTLR_EL1).unwrap(), 0);
        let mut fresh = Arm64KvmBackend::new(crate::LiveKvm::new().unwrap());
        fresh.set_policy(&crate::Arm64Policy::default()).unwrap();
        assert_eq!(backend.register_identity, fresh.register_identity);
        fresh.restore(&baseline).unwrap();
        assert_eq!(fresh.save().unwrap().core.pc, baseline.core.pc);
    }

    #[test]
    fn excluded_aarch32_modes_are_rejected_without_mutation() {
        let mut backend = Arm64KvmBackend::new(fake());
        backend.set_policy(&crate::Arm64Policy::default()).unwrap();
        let baseline = backend.save().unwrap();
        for mode in 0..32 {
            if matches!(mode, 0 | 4 | 5) {
                continue;
            }
            for spsr in [false, true] {
                let mut invalid = baseline;
                if spsr {
                    invalid.core.spsr_el1 = mode;
                } else {
                    invalid.core.pstate = mode;
                }
                let calls = backend.kvm.calls.clone();
                let regs = backend.kvm.regs.clone();
                assert!(backend.restore(&invalid).is_err());
                assert_eq!(backend.kvm.calls, calls);
                assert_eq!(backend.kvm.regs, regs);
                let id = core_reg(if spsr { CORE_SPSR_EL1 } else { CORE_PSTATE });
                backend.kvm.regs.insert(id, mode);
                assert!(matches!(
                    backend.save(),
                    Err(BackendError::Unsupported { .. })
                ));
                assert_eq!(backend.kvm.regs[&id], mode);
                backend.kvm.regs.insert(
                    id,
                    if spsr {
                        baseline.core.spsr_el1
                    } else {
                        baseline.core.pstate
                    },
                );
            }
        }
        assert_eq!(backend.save().unwrap(), baseline);
        backend.restore(&baseline).unwrap();
        backend.kvm.push_run(KvmRunView {
            exit_reason: KVM_EXIT_ARM_WFX_PLACEHOLDER,
            ..Default::default()
        });
        assert!(matches!(
            backend.run().unwrap(),
            Exit::Common(CommonExit::Idle)
        ));
    }

    #[test]
    #[cfg(not(miri))]
    fn captured_sysreg_classification_matches_saved_registers() {
        let actual = (0..=u16::MAX)
            .map(sysreg)
            .filter(|&id| disposition(id) == Some(Disposition::Captured))
            .collect::<std::collections::BTreeSet<_>>();
        let mut expected = SYSREGS
            .iter()
            .map(|&(id, _)| id)
            .collect::<std::collections::BTreeSet<_>>();
        for index in 0..16 {
            expected.extend([dbgbvr(index), dbgbcr(index), dbgwvr(index), dbgwcr(index)]);
        }
        expected.extend([
            MDCCINT_EL1,
            MDSCR_EL1,
            CNTV_CTL_EL0,
            CNTV_CVAL_EL0,
            CNTVCT_EL0,
        ]);
        assert_eq!(actual, expected);
    }

    #[test]
    fn measured_kvm_register_inventory_has_explicit_dispositions() {
        let entries = validate(fake_registers()).unwrap();
        assert_eq!(entries.len(), 257);
        let count = |kind| entries.iter().filter(|(_, found)| *found == kind).count();
        assert_eq!(count(Disposition::Captured), 159);
        assert_eq!(count(Disposition::ExcludedAarch32), 8);
        assert_eq!(count(Disposition::ExcludedPhysicalCounter), 1);
        assert_eq!(count(Disposition::OsLock), 1);
        for (id, kind) in entries {
            assert_eq!(disposition(id), Some(kind));
        }
    }

    #[test]
    fn unknown_missing_duplicate_and_failed_inventory_leave_policy_usable() {
        for case in 0..7 {
            let mut backend = Arm64KvmBackend::new(fake());
            backend.set_policy(&crate::Arm64Policy::default()).unwrap();
            let before = backend.save().unwrap();
            let calls = backend.kvm.calls.clone();
            let regs = backend.kvm.regs.clone();
            let mut ids = fake_registers();
            match case {
                0 => ids.push(sysreg(0xffff)),
                1 => ids.push(core_reg(82)),
                2 => ids.push(KVM_REG_ARM_VENDOR_HYP_BMAP_2 + 1),
                3 => ids.push(DEMUX | 14),
                4 => ids.retain(|&id| id != core_reg(CORE_PC)),
                5 => ids.push(ids[0]),
                6 => backend.kvm.register_inventory_failure = true,
                _ => unreachable!(),
            }
            backend.kvm.register_inventory = Some(ids);
            assert!(
                backend.set_policy(&crate::Arm64Policy::default()).is_err(),
                "case {case}"
            );
            assert!(backend.save().is_err(), "capture case {case}");
            assert!(backend.restore(&before).is_err(), "restore case {case}");
            assert!(!backend.state_poisoned, "case {case}");
            assert_eq!(backend.kvm.calls, calls, "case {case}");
            assert_eq!(backend.kvm.regs, regs, "case {case}");
            backend.kvm.register_inventory = None;
            backend.kvm.register_inventory_failure = false;
            assert_eq!(backend.save().unwrap(), before, "case {case}");
            backend.kvm.push_run(KvmRunView {
                exit_reason: KVM_EXIT_ARM_WFX_PLACEHOLDER,
                ..Default::default()
            });
            assert!(matches!(
                backend.run().unwrap(),
                Exit::Common(CommonExit::Idle)
            ));
        }
    }

    #[test]
    fn incoherent_policy_metadata_refuses_before_writes() {
        for (encoding, value, missing) in [
            (0xc081, 0, false),
            (0xffff, 0, false),
            (0xc01b, 1, false),
            (0xc006, 0, true),
        ] {
            let mut backend = Arm64KvmBackend::new(fake());
            backend.set_policy(&crate::Arm64Policy::default()).unwrap();
            let baseline = backend.save().unwrap();
            if missing {
                backend.kvm.register_inventory = Some(
                    fake_registers()
                        .into_iter()
                        .filter(|&id| id != sysreg(encoding))
                        .collect(),
                );
            }
            let policy = crate::Arm64Policy {
                id_regs: crate::IdRegModel {
                    regs: [(u32::from(encoding), value)].into_iter().collect(),
                },
                ..Default::default()
            };
            let calls = backend.kvm.calls.clone();
            let regs = backend.kvm.regs.clone();
            assert!(backend.set_policy(&policy).is_err());
            assert!(!backend.state_poisoned);
            assert_eq!(backend.kvm.calls, calls);
            assert_eq!(backend.kvm.regs, regs);
            backend.kvm.register_inventory = None;
            assert_eq!(backend.save().unwrap(), baseline);
        }
    }

    #[test]
    fn identity_binds_native_ids_cache_and_schema_but_not_captured_values() {
        let mut kvm = fake();
        let entries = validate(fake_registers()).unwrap();
        let baseline = identity(&kvm, &entries, true).unwrap();
        let mut reversed = fake_registers();
        reversed.reverse();
        assert_eq!(
            identity(&kvm, &validate(reversed).unwrap(), true).unwrap(),
            baseline
        );
        for id in [core_reg(0), sysreg(0xe180), PHYSICAL_COUNTER] {
            kvm.set_one_reg(id, 0xfedcba).unwrap();
            assert_eq!(identity(&kvm, &entries, true).unwrap(), baseline);
        }
        for id in [sysreg(0xc006), sysreg(0xc801), sysreg(0xc807)] {
            kvm.set_one_reg(id, 42).unwrap();
            assert_ne!(identity(&kvm, &entries, true).unwrap(), baseline);
            kvm.set_one_reg(id, 0).unwrap();
        }
        kvm.set_one_reg32(DEMUX, 42).unwrap();
        assert_ne!(identity(&kvm, &entries, true).unwrap(), baseline);
    }

    #[test]
    fn fixed_state_can_be_reset_but_reserved_ids_cannot_be_promoted() {
        let mut kvm = fake();
        let entries = validate(fake_registers()).unwrap();
        let baseline = identity(&kvm, &entries, true).unwrap();
        for id in [ACTLR_EL1, DISR_EL1, CNTP_CTL_EL0, CNTP_CVAL_EL0] {
            kvm.set_one_reg(id, 1).unwrap();
            assert!(identity(&kvm, &entries, true).is_err());
            assert_eq!(identity(&kvm, &entries, false).unwrap(), baseline);
            kvm.set_one_reg(id, 0).unwrap();
        }
        kvm.set_one_reg(OSLSR_EL1, 10).unwrap();
        assert!(identity(&kvm, &entries, true).is_err());
        assert_eq!(identity(&kvm, &entries, false).unwrap(), baseline);
        kvm.set_one_reg(OSLSR_EL1, 8).unwrap();
        kvm.set_one_reg(sysreg(RESERVED_IDS[0]), 1).unwrap();
        assert!(identity(&kvm, &entries, false).is_err());
    }

    #[test]
    fn immutable_register_drift_and_read_failure_refuse_capture() {
        let mut backend = Arm64KvmBackend::new(fake());
        backend.set_policy(&crate::Arm64Policy::default()).unwrap();
        backend.kvm.set_one_reg(sysreg(0xc006), 1).unwrap();
        assert!(backend.save().is_err());
        backend.kvm.set_one_reg(sysreg(0xc006), 0).unwrap();
        assert!(backend.save().is_ok());
        backend.kvm.fail_reg_read = Some(sysreg(0xc006));
        assert!(backend.save().is_err());
    }
}
