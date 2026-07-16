// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arm64 CPU-contract **policy skeleton** (`tasks/112` non-goal 5).
//!
//! The x86 contract (`docs/cpu-msr-contract.toml`, the `vendor::x86::contract`
//! module) is the **rigor template, not the content**: the ARM analogue is a
//! new document — a frozen synthetic `ID_AA64*` model plus a default-deny
//! trapped-sysreg table (`docs/ARCH-BOUNDARY.md` §B, ARM row) — and writing it
//! is port work informed by AA-6's enforcement-mechanism truth table. This
//! module supplies only the *shape*: an installable [`Arm64Policy`] whose row
//! sets are empty (`TODO(AA-6)`), and a deterministic policy hash so the
//! snapshot contract-mismatch check works end to end from day one.
//!
//! Default-deny is the **posture**, not the row count: an ID register absent
//! from the model is unfrozen only in the sense that no ruling exists yet, and
//! nothing here claims enforcement completeness. The trap *enforcement* is the
//! AA-3 patched backend's (`TODO(patched-abi)`).

use sha2::{Digest, Sha256};
use vmm_backend::{Arm64Policy, IdRegModel, SysregTrapPolicy};

/// The installable arm64 policy skeleton: an empty frozen-ID model and an
/// empty trap table (`TODO(AA-6)`: the contract document's row sets).
pub fn policy() -> Arm64Policy {
    Arm64Policy {
        id_regs: IdRegModel::default(),
        sysreg_traps: SysregTrapPolicy::default(),
    }
}

/// SHA-256 over the canonical encoding of the installed policy — the arm64
/// snapshot's `contract_hash` anchor. Two builds whose policy rows differ
/// stamp different hashes, so a snapshot taken under one contract skeleton is
/// refused by a VMM enforcing another (the same anti-drift role as the x86
/// `contract_hash`, INTEGRATION.md §4). The domain-separation prefix names the
/// skeleton explicitly so the hash can never collide with a ratified ARM
/// contract document's (which will hash its own canonical form, AA-6/port
/// work).
pub fn contract_hash() -> [u8; 32] {
    let p = policy();
    let mut h = Sha256::new();
    h.update(b"harmony-arm64-contract-skeleton-v0\0");
    // Canonical encoding: sorted (BTreeMap/BTreeSet) rows, little-endian
    // fixed-width fields, length-prefixed sections — deterministic (rule #4).
    h.update((p.id_regs.regs.len() as u64).to_le_bytes());
    for (enc, val) in &p.id_regs.regs {
        h.update(enc.to_le_bytes());
        h.update(val.to_le_bytes());
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
    fn policy_skeleton_is_empty_and_default_deny_shaped() {
        let p = policy();
        assert!(p.id_regs.regs.is_empty(), "rows are AA-6's, not guessed");
        assert!(p.sysreg_traps.trapped.is_empty());
    }

    #[test]
    fn contract_hash_is_deterministic_and_row_sensitive() {
        assert_eq!(contract_hash(), contract_hash());
        // A policy with a row must hash differently than the empty skeleton —
        // the anti-drift property the snapshot check relies on.
        let mut p = policy();
        p.id_regs.regs.insert(0x0018_0000, 0x1122);
        let mut h = Sha256::new();
        h.update(b"harmony-arm64-contract-skeleton-v0\0");
        h.update(1u64.to_le_bytes());
        h.update(0x0018_0000u32.to_le_bytes());
        h.update(0x1122u64.to_le_bytes());
        h.update(0u64.to_le_bytes());
        let with_row: [u8; 32] = h.finalize().into();
        assert_ne!(contract_hash(), with_row);
    }
}
