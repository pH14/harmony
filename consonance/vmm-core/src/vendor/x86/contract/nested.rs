// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use vmm_backend::{CpuidEntry, CpuidModel, MsrFilter, MsrRange, arch::x86::NestedFormat};

use super::parse::NestedHostPolicy;
use super::{MsrDisposition, contract_hash, cpuid_model, msr_filter_allow};

fn policy() -> &'static NestedHostPolicy {
    static POLICY: OnceLock<NestedHostPolicy> = OnceLock::new();
    POLICY.get_or_init(|| {
        NestedHostPolicy::load(include_str!("../../../../contracts/x86/nested-host.toml"))
    })
}

#[derive(Clone, Debug)]
pub struct NestedHostContract {
    msrs: BTreeMap<u32, u64>,
    hash: [u8; 32],
    format: NestedFormat,
    svm: Option<CpuidEntry>,
}

impl NestedHostContract {
    pub fn vmx(msrs: BTreeMap<u32, u64>) -> Result<Self, &'static str> {
        if msrs.keys().copied().collect::<Vec<_>>() != Self::vmx_indices() {
            return Err("nested-host requires the complete declared VMX MSR set");
        }
        Ok(Self::build(NestedFormat::Vmx, msrs, None))
    }

    pub fn svm(mut supported: CpuidEntry) -> Result<Self, &'static str> {
        if supported.leaf != 0x8000_000a
            || supported.subleaf != 0
            || supported.eax != 1
            || supported.ebx < 2
            || supported.ecx != 0
            || supported.edx & 1 == 0
        {
            return Err("nested-host requires SVM revision 1, ASIDs and nested paging");
        }
        supported.edx &= policy().svm_features_mask;
        supported.subleaf_significant = false;
        Ok(Self::build(
            NestedFormat::Svm,
            BTreeMap::from([
                (0xc001_0010, policy().svm_syscfg),
                (0xc001_0015, policy().svm_hwcr),
            ]),
            Some(supported),
        ))
    }

    fn build(format: NestedFormat, msrs: BTreeMap<u32, u64>, svm: Option<CpuidEntry>) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(policy().canonical());
        hasher.update(contract_hash());
        hasher.update((format as u16).to_le_bytes());
        if let Some(entry) = svm {
            for value in [entry.eax, entry.ebx, entry.ecx, entry.edx] {
                hasher.update(value.to_le_bytes());
            }
        }
        for (&index, &value) in &msrs {
            hasher.update(index.to_le_bytes());
            hasher.update(value.to_le_bytes());
        }
        Self {
            msrs,
            hash: hasher.finalize().into(),
            format,
            svm,
        }
    }

    pub fn vmx_cpuid_model() -> CpuidModel {
        let mut model = cpuid_model();
        model.entries.iter_mut().find(|e| e.leaf == 1).unwrap().ecx |= policy().leaf1_ecx_or;
        model
    }

    pub fn cpuid_model(&self) -> CpuidModel {
        if self.format == NestedFormat::Vmx {
            return Self::vmx_cpuid_model();
        }
        let mut model = cpuid_model();
        let vendor = (0x6874_7541, 0x444d_4163, 0x6974_6e65);
        for entry in &mut model.entries {
            match entry.leaf {
                0 | 0x8000_0000 => {
                    entry.ebx = vendor.0;
                    entry.ecx = vendor.1;
                    entry.edx = vendor.2;
                    if entry.leaf == 0x8000_0000 {
                        entry.eax = 0x8000_000a;
                    }
                }
                1 => {
                    entry.eax = policy().svm_signature;
                }
                0x8000_0001 => {
                    entry.eax = policy().svm_signature;
                    entry.ecx |= policy().svm_extended_ecx_or;
                }
                _ => {}
            }
        }
        model.entries.push(self.svm.unwrap());
        model
            .entries
            .sort_by_key(|entry| (entry.leaf, entry.subleaf));
        model
    }

    pub fn msr_filter(&self) -> MsrFilter {
        let mut filter = msr_filter_allow();
        let indices: &[u32] = match self.format {
            NestedFormat::Vmx => &[0x3a],
            NestedFormat::Svm => &policy().svm_indices,
        };
        for &base in indices {
            filter.allow_inkernel.push(MsrRange { base, count: 1 });
        }
        filter.allow_inkernel.sort_unstable();
        filter
    }

    pub fn format(&self) -> NestedFormat {
        self.format
    }

    pub fn vmx_indices() -> &'static [u32] {
        &policy().vmx_indices
    }

    pub fn feature_control() -> u64 {
        policy().feature_control
    }

    pub fn rdmsr_disposition(&self, index: u32) -> Option<MsrDisposition> {
        self.msrs
            .get(&index)
            .copied()
            .map(MsrDisposition::AllowFixed)
    }

    pub fn hash(&self) -> [u8; 32] {
        self.hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
    #[test]
    #[ignore = "requires nested VMX or SVM and NESTED_HOST_KERNEL / NESTED_HOST_INITRAMFS"]
    fn nested_restore_preserves_unsynchronized_vmcs_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let read = |name| -> Result<Vec<u8>, Box<dyn std::error::Error>> {
            Ok(std::fs::read(std::env::var(name)?)?)
        };
        let kernel = read("NESTED_HOST_KERNEL")?;
        let initramfs = read("NESTED_HOST_INITRAMFS")?;
        let cmdline = "console=ttyS0 panic=-1 reboot=t tsc=reliable no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable harmony_pvclock noxsaveopt noxsaves LD_BIND_NOW=1 harmony_nested_cache_check";
        let mut vmm = crate::vendor::x86::bringup::boot_linux_nested_host_virtual_time(
            &kernel,
            &initramfs,
            256 << 20,
            cmdline,
            42,
        )?;
        vmm.defer_virtual_time_checkpoint_hashes()?;
        let advance = |vmm: &mut crate::vmm::Vmm<vmm_backend::KvmBackend>,
                       step|
         -> Result<(), Box<dyn std::error::Error>> {
            let marker = format!("NESTED_CACHE_STEP={step}\r\n");
            for _ in 0..5_000_000 {
                let progress = vmm.step()?;
                if vmm.serial().ends_with(marker.as_bytes()) {
                    return Ok(());
                }
                if progress != crate::vmm::Step::Continued {
                    return Err(format!("cache fixture stopped: {progress:?}").into());
                }
            }
            Err("cache fixture exceeded step budget".into())
        };
        advance(&mut vmm, 3)?;
        let state = vmm.save_vm_state()?;
        let memory = vmm.guest_memory().to_vec();
        let saved = state
            .nested_state
            .as_deref()
            .ok_or("nested state missing")?;
        if vmm_backend::arch::x86::NestedFormat::from_state(saved)?
            == vmm_backend::arch::x86::NestedFormat::Vmx
        {
            assert!(saved.len() > 128, "fixture has no loaded VMCS");
        }
        advance(&mut vmm, 5)?;
        vmm.retire_pending_completion()?;
        vmm.restore_snapshot(&memory, &state)?;
        let cpu = vmm.vcpu_record()?;
        let restored = cpu
            .nested_state
            .as_deref()
            .ok_or("restored nested state missing")?;
        if saved != restored {
            let first = saved.iter().zip(restored).position(|(a, b)| a != b);
            return Err(format!(
                "nested restore corrupted VMCS fields: saved={} restored={} first_difference={first:?}",
                saved.len(), restored.len()
            ).into());
        }
        println!("NESTED_CACHE_RESTORE vmcs_fields=pass");
        Ok(())
    }

    #[test]
    fn nested_snapshot_publication_retains_every_live_backend_byte() {
        use crate::vmm::{GuestRam, Vmm};
        use vmm_backend::{MockBackend, VcpuState};
        let mut bytes = vec![0x5a; vmm_backend::arch::x86::VMX_NESTED_MAX_LEN];
        bytes[..4].fill(0);
        let size = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        bytes[8..16].copy_from_slice(&0x1000_u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&0x2000_u64.to_le_bytes());
        let mut backend = MockBackend::new();
        backend.set_state(VcpuState {
            nested_state: Some(bytes.clone()),
            ..Default::default()
        });
        let mut vmm = Vmm::new(backend, GuestRam::new(0x10000).unwrap());
        vmm.devices.nested_host = Some(
            NestedHostContract::vmx(
                NestedHostContract::vmx_indices()
                    .iter()
                    .map(|&index| (index, 0))
                    .collect(),
            )
            .unwrap(),
        );
        assert_eq!(vmm.save_vm_state().unwrap().nested_state, Some(bytes));
    }

    #[test]
    fn nested_host_adds_only_vmx_to_cpuid() {
        let ordinary = cpuid_model();
        let mut nested = NestedHostContract::vmx_cpuid_model();
        nested.entries.iter_mut().find(|e| e.leaf == 1).unwrap().ecx &= !(1 << 5);
        assert_eq!(ordinary, nested);
        assert_eq!(NestedHostContract::feature_control(), 5);
    }

    #[test]
    fn nested_host_identity_covers_every_host_msr() {
        let msrs: BTreeMap<_, _> = NestedHostContract::vmx_indices()
            .iter()
            .copied()
            .map(|i| (i, u64::from(i)))
            .collect();
        let original = NestedHostContract::vmx(msrs.clone()).unwrap();
        assert_ne!(original.hash(), contract_hash());
        for &index in NestedHostContract::vmx_indices() {
            let mut changed = msrs.clone();
            *changed.get_mut(&index).unwrap() ^= 1;
            assert_ne!(
                original.hash(),
                NestedHostContract::vmx(changed).unwrap().hash()
            );
            let mut missing = msrs.clone();
            missing.remove(&index);
            assert!(NestedHostContract::vmx(missing).is_err());
            assert_eq!(
                original.rdmsr_disposition(index),
                Some(MsrDisposition::AllowFixed(u64::from(index)))
            );
            assert_eq!(
                super::super::rdmsr_disposition(index),
                MsrDisposition::DenyGp
            );
        }
        let mut extra = msrs;
        extra.insert(0x492, 1);
        assert!(NestedHostContract::vmx(extra).is_err());
    }

    fn supported_svm() -> CpuidEntry {
        CpuidEntry {
            leaf: 0x8000_000a,
            eax: 1,
            ebx: 8,
            edx: 0xffff_ffff,
            ..Default::default()
        }
    }

    #[test]
    fn svm_exposes_only_declared_capabilities_and_binds_every_exposed_host_value() {
        let supported = supported_svm();
        let contract = NestedHostContract::svm(supported).unwrap();
        assert_ne!(contract.hash(), contract_hash());
        let model = contract.cpuid_model();
        let leaf = |number| *model.entries.iter().find(|e| e.leaf == number).unwrap();
        assert_eq!(
            (leaf(0).ebx, leaf(0).edx, leaf(0).ecx),
            (0x6874_7541, 0x6974_6e65, 0x444d_4163)
        );
        assert_eq!(leaf(1).eax, policy().svm_signature);
        assert_eq!(leaf(1).ecx & (1 << 5), 0);
        assert_eq!(leaf(0x8000_0000).eax, 0x8000_000a);
        assert_ne!(leaf(0x8000_0001).ecx & (1 << 2), 0);
        assert_eq!(leaf(0x8000_000a).edx, policy().svm_features_mask);
        assert_eq!(leaf(0x8000_000a).ebx, 8);
        for bit in [3, 5, 6, 7] {
            let changed = CpuidEntry {
                edx: supported.edx ^ (1 << bit),
                ..supported
            };
            assert_ne!(
                contract.hash(),
                NestedHostContract::svm(changed).unwrap().hash()
            );
        }
        let changed = CpuidEntry {
            ebx: 9,
            ..supported
        };
        assert_ne!(
            contract.hash(),
            NestedHostContract::svm(changed).unwrap().hash()
        );
        let hidden = CpuidEntry {
            edx: supported.edx & !(1 << 4),
            ..supported
        };
        assert_eq!(
            contract.hash(),
            NestedHostContract::svm(hidden).unwrap().hash()
        );
        let original = msr_filter_allow().allow_indices().collect::<Vec<_>>();
        let nested = contract.msr_filter().allow_indices().collect::<Vec<_>>();
        assert_eq!(
            nested
                .iter()
                .copied()
                .filter(|i| !original.contains(i))
                .collect::<Vec<_>>(),
            policy().svm_indices
        );
        assert!(!nested.contains(&0x3a));
        assert!(!nested.contains(&0xc001_0015));
        assert!(!nested.contains(&0xc001_0010));
        assert_eq!(
            contract.rdmsr_disposition(0xc001_0010),
            Some(MsrDisposition::AllowFixed(0))
        );
        assert_eq!(
            super::super::rdmsr_disposition(0xc001_0010),
            MsrDisposition::DenyGp
        );
        for index in [0xc001_0010, 0xc001_0015] {
            assert_eq!(
                super::super::wrmsr_disposition(index, 0),
                MsrDisposition::DenyGp
            );
        }
        assert_eq!(
            contract.rdmsr_disposition(0xc001_0015),
            Some(MsrDisposition::AllowFixed(1 << 24))
        );
        assert_eq!(
            super::super::rdmsr_disposition(0xc001_0015),
            MsrDisposition::DenyGp
        );
        for index in NestedHostContract::vmx_indices() {
            assert_eq!(contract.rdmsr_disposition(*index), None);
        }
        for entry in [
            CpuidEntry {
                eax: 0,
                ..supported
            },
            CpuidEntry {
                ebx: 1,
                ..supported
            },
            CpuidEntry {
                ecx: 1,
                ..supported
            },
            CpuidEntry {
                edx: 0,
                ..supported
            },
        ] {
            assert!(NestedHostContract::svm(entry).is_err());
        }
    }

    #[test]
    fn nested_snapshot_vendor_mixing_rejects_before_mutating_ram_or_cpu() {
        use crate::vmm::{GuestRam, Vmm};
        use vmm_backend::MockBackend;
        let vmx = NestedHostContract::vmx(
            NestedHostContract::vmx_indices()
                .iter()
                .map(|&index| (index, 0))
                .collect(),
        )
        .unwrap();
        let svm = NestedHostContract::svm(supported_svm()).unwrap();
        let mut machines = [vmx, svm].map(|contract| {
            let mut backend = MockBackend::new();
            let cpu = vmm_backend::VcpuState {
                nested_state: Some(vmm_backend::arch::x86::inactive_nested_state(
                    contract.format(),
                )),
                ..Default::default()
            };
            backend.set_state(cpu);
            let mut vmm = Vmm::new(backend, GuestRam::new(0x10000).unwrap());
            vmm.devices.nested_host = Some(contract);
            vmm
        });
        let states = [
            machines[0].save_vm_state().unwrap(),
            machines[1].save_vm_state().unwrap(),
        ];
        for (index, machine) in machines.iter_mut().enumerate() {
            let before = machine.state_hash().unwrap();
            let mut wrong = states[1 - index].clone();
            assert!(machine.restore_vm_state(&wrong).is_err());
            wrong.contract_hash = states[index].contract_hash;
            assert!(machine.restore_vm_state(&wrong).is_err());
            assert_eq!(machine.state_hash().unwrap(), before);
        }
    }

    #[test]
    fn ordinary_and_nested_host_snapshots_reject_each_other_without_mutation() {
        use crate::snapshot::SnapshotError;
        use crate::vmm::{GuestRam, Vmm, VmmError};
        use vmm_backend::MockBackend;

        let mut ordinary = Vmm::new(MockBackend::new(), GuestRam::new(0x10000).unwrap());
        let mut backend = MockBackend::new();
        let mut cpu = vmm_backend::VcpuState::default();
        let mut bytes = vec![0; 128];
        bytes[4..8].copy_from_slice(&128_u32.to_le_bytes());
        bytes[8..24].fill(0xff);
        cpu.nested_state = Some(bytes);
        backend.set_state(cpu);
        let mut nested = Vmm::new(backend, GuestRam::new(0x10000).unwrap());
        nested.devices.nested_host = Some(
            NestedHostContract::vmx(
                NestedHostContract::vmx_indices()
                    .iter()
                    .copied()
                    .map(|i| (i, u64::from(i)))
                    .collect(),
            )
            .unwrap(),
        );
        let ordinary_state = ordinary.save_vm_state().unwrap();
        let nested_state = nested.save_vm_state().unwrap();
        for (target, source) in [
            (&mut ordinary, &nested_state),
            (&mut nested, &ordinary_state),
        ] {
            let before = target.state_hash().unwrap();
            assert!(matches!(
                target.restore_vm_state(source),
                Err(VmmError::Snapshot(SnapshotError::ContractMismatch))
            ));
            assert_eq!(target.state_hash().unwrap(), before);
        }
    }
}
