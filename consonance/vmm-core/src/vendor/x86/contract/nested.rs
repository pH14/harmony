// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use vmm_backend::{CpuidModel, MsrFilter, MsrRange};

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
}

impl NestedHostContract {
    pub fn new(msrs: BTreeMap<u32, u64>) -> Result<Self, &'static str> {
        if msrs.keys().copied().collect::<Vec<_>>() != Self::vmx_indices() {
            return Err("nested-host requires the complete declared VMX MSR set");
        }
        let mut hasher = Sha256::new();
        hasher.update(policy().canonical());
        hasher.update(contract_hash());
        for (&index, &value) in &msrs {
            hasher.update(index.to_le_bytes());
            hasher.update(value.to_le_bytes());
        }
        Ok(Self {
            msrs,
            hash: hasher.finalize().into(),
        })
    }

    pub fn cpuid_model() -> CpuidModel {
        let mut model = cpuid_model();
        let leaf1 = model.entries.iter_mut().find(|e| e.leaf == 1).unwrap();
        leaf1.ecx |= policy().leaf1_ecx_or;
        model
    }

    pub fn msr_filter() -> MsrFilter {
        let mut filter = msr_filter_allow();
        filter.allow_inkernel.push(MsrRange {
            base: 0x3a,
            count: 1,
        });
        filter.allow_inkernel.sort_unstable();
        filter
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

    #[test]
    fn nested_host_adds_only_vmx_to_cpuid() {
        let ordinary = cpuid_model();
        let mut nested = NestedHostContract::cpuid_model();
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
        let original = NestedHostContract::new(msrs.clone()).unwrap();
        assert_ne!(original.hash(), contract_hash());
        for &index in NestedHostContract::vmx_indices() {
            let mut changed = msrs.clone();
            *changed.get_mut(&index).unwrap() ^= 1;
            assert_ne!(
                original.hash(),
                NestedHostContract::new(changed).unwrap().hash()
            );
            let mut missing = msrs.clone();
            missing.remove(&index);
            assert!(NestedHostContract::new(missing).is_err());
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
        assert!(NestedHostContract::new(extra).is_err());
    }

    #[test]
    fn ordinary_and_nested_host_snapshots_reject_each_other_without_mutation() {
        use crate::snapshot::SnapshotError;
        use crate::vmm::{GuestRam, Vmm, VmmError};
        use vmm_backend::MockBackend;

        let mut ordinary = Vmm::new(MockBackend::new(), GuestRam::new(0x10000).unwrap());
        let mut nested = Vmm::new(MockBackend::new(), GuestRam::new(0x10000).unwrap());
        nested.devices.nested_host = Some(
            NestedHostContract::new(
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
