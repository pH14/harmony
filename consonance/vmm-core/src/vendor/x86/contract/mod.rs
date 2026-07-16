// SPDX-License-Identifier: AGPL-3.0-or-later
//! CPUID model + MSR-filter **policy** (the *what*), built from the ratified
//! `docs/CPU-MSR-CONTRACT.md` data (its canonical mirror
//! `docs/cpu-msr-contract.toml`, embedded at compile time).
//!
//! `vmm-core` owns the policy; the install *mechanism* (`KVM_SET_CPUID2`,
//! `KVM_X86_SET_MSR_FILTER`, `KVM_CAP_X86_USER_SPACE_MSR`) is KVM-specific and
//! lives **below the trait** in `vmm-backend`. These functions produce
//! backend-agnostic values ([`vmm_backend::CpuidModel`] / [`vmm_backend::MsrFilter`]
//! / [`MsrDisposition`]) that [`crate::bringup::boot`] hands to the backend
//! through the trait. [`contract_hash`] is the SHA-256 of the §6 canonical
//! serialization of these same tables, so the policy can never drift from the
//! ratified contract.
//!
//! The contract is ingested as a **checked-in TOML embedded with `include_str!`**
//! and parsed once at first use (no `toml` runtime dependency, no `build.rs`
//! codegen, no second hand-maintained copy — the parser that loads the tables is
//! the same code the canonical serializer emits from). See `parse` / `canonical`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use vmm_backend::{CpuidEntry, CpuidModel, MsrFilter, MsrRange};

mod canonical;
mod parse;

use parse::{Contract, Subleaf, VendorId};

/// The ratified **Intel** contract artifact (`docs/cpu-msr-contract.toml`, the
/// `det-cfl-v1`/`GenuineIntel` column), embedded at compile time. The path is
/// relative to this source file; a docs/ move breaks the build loudly (intended —
/// the contract is a hard input, not optional).
const CONTRACT_TOML: &str = include_str!("../../../../../../docs/cpu-msr-contract.toml");

/// The **AMD draft** column (`docs/cpu-msr-contract-amd-draft.toml`, the
/// `det-zenN-v1`/`AuthenticAMD` column), embedded **only under `cfg(test)`**. This
/// is the structural draft-only guard (Deliverable 7/8): no live VM construction
/// path can name this constant, so the draft is unreachable from `boot`/`bringup`.
/// It is loadable + canonicalizable (its own `contract_hash`) but wired into no
/// enforcement path — every enforcement cell is `verify-on-silicon` pending AE-4.
#[cfg(test)]
const CONTRACT_AMD_DRAFT_TOML: &str =
    include_str!("../../../../../../docs/cpu-msr-contract-amd-draft.toml");

/// The parsed **Intel** contract (the live policy path), built once on first use.
/// Loaded under the `GenuineIntel` vendor axis: a vendor-mismatched or mixed-vendor
/// Intel file is a build bug, caught loudly here (trusted compile-time data — the
/// `expect` matches `parse`'s embedded-contract discipline).
fn contract() -> &'static Contract {
    static CACHE: OnceLock<Contract> = OnceLock::new();
    CACHE.get_or_init(|| {
        Contract::load(CONTRACT_TOML, VendorId::GenuineIntel)
            .expect("embedded Intel contract must declare vendor = \"GenuineIntel\"")
    })
}

/// The parsed **AMD draft** column, built once on first use — `cfg(test)` only, so
/// it never reaches a live policy path (Deliverable 7). Loaded under the
/// `AuthenticAMD` axis, with the mixed-vendor guard active.
#[cfg(test)]
fn contract_amd_draft() -> &'static Contract {
    static CACHE: OnceLock<Contract> = OnceLock::new();
    CACHE.get_or_init(|| {
        Contract::load(CONTRACT_AMD_DRAFT_TOML, VendorId::AuthenticAMD)
            .expect("embedded AMD draft contract must declare vendor = \"AuthenticAMD\"")
    })
}

// ---------------------------------------------------------------------------
// MSR user-space exit mask (CPU-MSR-CONTRACT §1).
// ---------------------------------------------------------------------------

/// `KVM_MSR_EXIT_REASON_FILTER` bit value (bit 0). Written `1` rather than `1 << 0`
/// so the shift operator carries no equivalent (`1 << 0` ≡ `1 >> 0`) mutant.
pub const MSR_EXIT_REASON_FILTER: u64 = 1;
/// `KVM_MSR_EXIT_REASON_UNKNOWN` bit value.
pub const MSR_EXIT_REASON_UNKNOWN: u64 = 1 << 1;
/// `KVM_MSR_EXIT_REASON_INVAL` bit value.
pub const MSR_EXIT_REASON_INVAL: u64 = 1 << 2;

/// The mask `vmm-backend` must enable on `KVM_CAP_X86_USER_SPACE_MSR` **before
/// installing the MSR filter** (CPU-MSR-CONTRACT §1; api.rst §4.97 ordering):
/// `FILTER | UNKNOWN | INVAL`. Enabling the cap first is load-bearing — otherwise
/// a denied/unknown/invalid MSR becomes a silent in-kernel `#GP` instead of a loud
/// `KVM_EXIT_X86_RDMSR/WRMSR`.
pub const USER_SPACE_MSR_MASK: u64 =
    MSR_EXIT_REASON_FILTER | MSR_EXIT_REASON_UNKNOWN | MSR_EXIT_REASON_INVAL;

// ---------------------------------------------------------------------------
// MSR disposition vocabulary (CPU-MSR-CONTRACT §3).
// ---------------------------------------------------------------------------

/// Per-direction disposition of an MSR access (the §3 vocabulary the skeleton
/// needs).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MsrDisposition {
    /// Architecturally guest-writable; KVM virtualizes it — placed in the filter
    /// **allow** set, so it is serviced in-kernel and never reaches a userspace
    /// exit.
    AllowStateful,
    /// Read returns this constant (read-only rows); write is denied.
    AllowFixed(u64),
    /// `emulate-vtime` rows (CPU-MSR-CONTRACT §3): `MSR_IA32_TSC` (0x10) and
    /// `MSR_IA32_TSC_ADJUST` (0x3b), read **and** write — serviced from V-time.
    /// V-time is not wired in this skeleton, so an actual `0x10`/`0x3b` access is
    /// a loud `ContractViolation` until V-time lands; the audited M1/M2 payloads
    /// touch neither. Kept an explicit variant so folding it into
    /// `AllowFixed`/`DenyGp` cannot silently break the contract.
    EmulateVtime,
    /// Trapped, logged loudly, then `#GP` injected.
    DenyGp,
    /// Write dropped after a loud log (never silent); the read side is never this.
    DenyIgnoreWrite,
}

/// Map a `(token, param)` pair from the contract to an [`MsrDisposition`].
fn disposition_of(token: &str, param: Option<&str>) -> MsrDisposition {
    match token {
        "allow-stateful" => MsrDisposition::AllowStateful,
        "allow-fixed" => MsrDisposition::AllowFixed(param.map(hex64).unwrap_or(0)),
        "emulate-vtime" => MsrDisposition::EmulateVtime,
        "deny-ignore-write" => MsrDisposition::DenyIgnoreWrite,
        // `deny-gp` and any unexpected token fail closed to deny-gp.
        _ => MsrDisposition::DenyGp,
    }
}

/// Parse a `"0x...."`/bare-hex 64-bit param.
fn hex64(s: &str) -> u64 {
    u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).unwrap_or(0)
}

/// The host-baseline expectations vmm-core enforces at VM start (CPU-MSR-CONTRACT
/// §1.1/§1.2), extracted from the ratified contract for the [`crate::hostassert`]
/// checker. The §6 `guest-ucode-rev` and `cr4-force-reserved` records are part of
/// the hashed canonical form but are **not** host probes — one is the
/// guest-visible BIOS_SIGN_ID fake, the other a guest-CR4 configuration invariant
/// enforced by the frozen CPUID model — so they are not surfaced here.
///
/// Gated to the box (Linux/x86-64, not Miri): only the live `hostassert::probe`
/// consumes it, and only there is a physical host present to assert against.
#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
pub(crate) struct HostExpectations {
    /// `06_9e_0c` — required host CPUID(1) family/model/stepping (hex `ff_mm_ss`).
    pub family_model_stepping: &'static str,
    /// The physical host microcode revision (IA32_BIOS_SIGN_ID revision field),
    /// fleet-pinned; **distinct** from the guest-visible `guest-ucode-rev`.
    pub microcode_rev: u64,
    /// `0x0000ffff` — required host FXSAVE-area `MXCSR_MASK`.
    pub mxcsr_mask: u32,
    /// Minimum host MAXPHYADDR (CPUID 0x8000_0008 EAX[7:0]).
    pub maxphyaddr_min: u32,
    /// Whether RTM must be made non-usable by the guest (host lacks RTM, or has
    /// `IA32_TSX_CTRL` to disable it).
    pub rtm_disabled: bool,
    /// Instructions the contract relies on faulting by **physical absence**.
    pub host_absent: &'static [String],
}

/// The parsed host-baseline expectations (`[host-assert]`), for [`crate::hostassert`].
#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
pub(crate) fn host_expectations() -> HostExpectations {
    let ha = &contract().host_assert;
    HostExpectations {
        family_model_stepping: ha.family_model_stepping.as_str(),
        microcode_rev: hex64(&ha.host_microcode_rev),
        mxcsr_mask: hex64(&ha.mxcsr_mask) as u32,
        maxphyaddr_min: ha.maxphyaddr_min as u32,
        rtm_disabled: ha.rtm_disabled,
        host_absent: &ha.host_absent,
    }
}

/// The per-index disposition table, built once: `index → (read, write)`.
type DispMap = BTreeMap<u32, (MsrDisposition, MsrDisposition)>;

fn disp_map() -> &'static DispMap {
    static CACHE: OnceLock<DispMap> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut map = DispMap::new();
        for row in &contract().msr {
            let read = disposition_of(&row.read, row.read_param.as_deref());
            let write = disposition_of(&row.write, row.write_param.as_deref());
            for idx in row.index.indices() {
                map.insert(idx, (read, write));
            }
        }
        map
    })
}

/// Compute the contractual disposition of a guest read of `index` (default
/// [`MsrDisposition::DenyGp`]).
pub fn rdmsr_disposition(index: u32) -> MsrDisposition {
    disp_map()
        .get(&index)
        .map_or(MsrDisposition::DenyGp, |(r, _)| *r)
}

/// Compute the contractual disposition of a guest write of `value` to `index`
/// (default [`MsrDisposition::DenyGp`]). `value` is carried for logging / future
/// value-dependent rows; no in-scope row branches on it.
pub fn wrmsr_disposition(index: u32, value: u64) -> MsrDisposition {
    let _ = value;
    disp_map()
        .get(&index)
        .map_or(MsrDisposition::DenyGp, |(_, w)| *w)
}

// ---------------------------------------------------------------------------
// CPUID model (CPU-MSR-CONTRACT §2).
// ---------------------------------------------------------------------------

/// The frozen CPUID model from §2 of the contract, in canonical (leaf, subleaf)
/// order, as [`vmm_backend::CpuidModel`] so it feeds straight into
/// [`vmm_backend::Backend::set_cpuid`]. Installed once via `KVM_SET_CPUID2` so
/// CPUID is answered **in-kernel** from this model (no host leaves inherited).
/// Masks `X2APIC` (CPUID.1:ECX[21]) and the TSC-deadline bit (CPUID.1:ECX[24])
/// and hides all PV leaves (`0x4000_00xx`) and the vPMU (leaf `0xA`), per R1.
///
/// This is the **frozen base only** — the three dynamic cells (OSXSAVE, the
/// `0xB`/`0x1F` level echo, the `0xD.0` XSAVE size) are recomputed in-kernel by
/// stock KVM (`kvm_update_cpuid_runtime`), so the base table is correct and no
/// CPUID exit fires; a backend surfacing a userspace `X86Exit::Cpuid` must overlay
/// them via [`resolve_cpuid`].
pub fn cpuid_model() -> CpuidModel {
    let c = contract();
    let mut entries = Vec::with_capacity(c.cpuid.len());
    for row in &c.cpuid {
        // Leaf ranges (the one PV/hypervisor zero block) install a single
        // representative entry at `lo`; KVM zero-fills the rest, hiding the range.
        let leaf = row.leaf.lo;
        let (subleaf, significant) = match row.subleaf {
            Subleaf::Single(s) => (s, c.leaf_entry_count(leaf) > 1),
            Subleaf::All => (0, false),
            Subleaf::AndUp(n) => (n, true),
            Subleaf::Range(lo, _) => (lo, true),
        };
        entries.push(CpuidEntry {
            leaf,
            subleaf,
            subleaf_significant: significant,
            eax: row.eax.base(),
            ebx: row.ebx.base(),
            ecx: row.ecx.base(),
            edx: row.edx.base(),
        });
    }
    CpuidModel { entries }
}

/// Overlay the three dynamic CPUID cells (see [`cpuid_model`]) onto the frozen
/// `base` entry when servicing a userspace `X86Exit::Cpuid`, from the guest's live
/// `CR4`/`XCR0` (`base.leaf`/`base.subleaf` select which rule applies). Never
/// called for stock `KvmBackend` (CPUID is in-kernel); it exists so the
/// patched/direct path stays contract-correct. Pure.
pub fn resolve_cpuid(base: CpuidEntry, cr4: u64, xcr0: u64) -> CpuidEntry {
    let mut e = base;
    match (base.leaf, base.subleaf) {
        // CPUID.1:ECX[27] (OSXSAVE) mirrors CR4.OSXSAVE (CR4 bit 18).
        (0x1, 0) => {
            let osxsave = ((cr4 >> 18) & 1) as u32;
            e.ecx = (e.ecx & !(1 << 27)) | (osxsave << 27);
        }
        // Leaf 0xB/0x1F: ECX[7:0] echoes the input subleaf, ECX[15:8] the level
        // type. The concrete in-table subleaves already encode this, so applying
        // it is idempotent there and correct for the past-the-end (dynamic) rows.
        (0xB | 0x1F, _) => {
            e.ecx = (base.subleaf & 0xFF) | (e.ecx & 0xFF00);
        }
        // Leaf 0xD.0:EBX is the XSAVE-area size for the live XCR0 (0x240 for
        // XCR0 ∈ {0x1, 0x3}; 0x340 once AVX (XCR0 bit 2) is enabled).
        (0xD, 0) => {
            e.ebx = if xcr0 & 0x4 != 0 { 0x340 } else { 0x240 };
        }
        _ => {}
    }
    e
}

// ---------------------------------------------------------------------------
// MSR filter allow set (CPU-MSR-CONTRACT §3 — the allow-stateful rows).
// ---------------------------------------------------------------------------

/// The MSR-filter allow set: exactly the `allow-stateful` rows — the only MSRs
/// KVM keeps servicing in-kernel — as [`vmm_backend::MsrFilter`] so it feeds
/// straight into [`vmm_backend::Backend::set_msr_filter`]. Every other disposition
/// is left out on purpose so the access surfaces to a userspace exit. Ranges are
/// canonical, sorted, and non-overlapping; the backend installs them under
/// `KVM_MSR_FILTER_DEFAULT_DENY` with both READ and WRITE flags (well within
/// KVM's 16-ranges-per-direction limit).
pub fn msr_filter_allow() -> MsrFilter {
    // Collect every bidirectional allow-stateful index.
    let mut indices: Vec<u32> = Vec::new();
    for row in &contract().msr {
        if row.read == "allow-stateful" && row.write == "allow-stateful" {
            indices.extend(row.index.indices());
        }
    }
    indices.sort_unstable();
    indices.dedup();

    // Coalesce consecutive indices into [base, base+count) ranges.
    let mut ranges: Vec<MsrRange> = Vec::new();
    for idx in indices {
        match ranges.last_mut() {
            Some(last) if last.base + last.count == idx => last.count += 1,
            _ => ranges.push(MsrRange {
                base: idx,
                count: 1,
            }),
        }
    }
    MsrFilter {
        allow_inkernel: ranges,
    }
}

// ---------------------------------------------------------------------------
// Contract hash (CPU-MSR-CONTRACT §6).
// ---------------------------------------------------------------------------

/// SHA-256 of the canonical serialized contract this policy was built from (§6
/// `contract_hash`). The bytes are the §6 canonical form emitted by
/// [`canonical::serialize`] from the same parsed tables the runtime policy uses,
/// so policy can never drift from the ratified contract.
///
/// As of v3 (det-cfl-v1) the §6 registry is committed: `docs/cpu-msr-contract.toml`
/// `[contract] contract_hash` carries the hash of exactly these bytes, and the
/// `contract_hash() == toml field` gate ([`tests::contract_hash_matches_committed_registry`])
/// is live and green.
pub fn contract_hash() -> [u8; 32] {
    let canonical = canonical::serialize(contract());
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn user_space_mask_is_filter_unknown_inval() {
        assert_eq!(
            USER_SPACE_MSR_MASK,
            MSR_EXIT_REASON_FILTER | MSR_EXIT_REASON_UNKNOWN | MSR_EXIT_REASON_INVAL
        );
        assert_eq!(USER_SPACE_MSR_MASK, 0b111);
    }

    #[test]
    fn msr_filter_allow_is_the_stateful_set() {
        let f = msr_filter_allow();
        // Sorted, non-overlapping, within KVM's 16-range limit.
        assert!(f.allow_inkernel.len() <= 16);
        for w in f.allow_inkernel.windows(2) {
            assert!(
                w[0].base + w[0].count <= w[1].base,
                "ranges sorted/disjoint"
            );
        }
        // EFER (0xc0000080) and CR_PAT (0x277) are allow-stateful; PLATFORM_ID
        // (0x17, allow-fixed) and IA32_TSC (0x10, emulate-vtime) are NOT.
        let covered = |idx: u32| f.allow_indices().any(|i| i == idx);
        assert!(covered(0xC000_0080), "EFER in-kernel");
        assert!(covered(0x277), "CR_PAT in-kernel");
        assert!(!covered(0x17), "PLATFORM_ID not in-kernel");
        assert!(!covered(0x10), "IA32_TSC not in-kernel");
    }

    #[test]
    fn msr_dispositions_match_contract() {
        // Default deny-gp for an unlisted index.
        assert_eq!(rdmsr_disposition(0xDEAD_BEEF), MsrDisposition::DenyGp);
        // allow-fixed returns its constant; write denied.
        assert_eq!(rdmsr_disposition(0x17), MsrDisposition::AllowFixed(0));
        assert_eq!(wrmsr_disposition(0x17, 0), MsrDisposition::DenyGp);
        // IA32_APICBASE (0x1b): read allow-fixed, write deny-ignore-write.
        assert_eq!(
            rdmsr_disposition(0x1B),
            MsrDisposition::AllowFixed(0xFEE0_0900)
        );
        assert_eq!(wrmsr_disposition(0x1B, 0), MsrDisposition::DenyIgnoreWrite);
        // 0x10 / 0x3b are emulate-vtime for BOTH directions (not allow/deny).
        for idx in [0x10u32, 0x3b] {
            assert_eq!(rdmsr_disposition(idx), MsrDisposition::EmulateVtime);
            assert_eq!(wrmsr_disposition(idx, 0), MsrDisposition::EmulateVtime);
        }
        // EFER is allow-stateful both ways.
        assert_eq!(
            rdmsr_disposition(0xC000_0080),
            MsrDisposition::AllowStateful
        );
        assert_eq!(
            wrmsr_disposition(0xC000_0080, 0),
            MsrDisposition::AllowStateful
        );
    }

    #[test]
    fn cpuid_model_masks_and_hides() {
        let m = cpuid_model();
        let leaf1 = m
            .entries
            .iter()
            .find(|e| e.leaf == 1 && e.subleaf == 0)
            .expect("leaf 1");
        // X2APIC (bit 21) and TSC-deadline (bit 24) masked off.
        assert_eq!(leaf1.ecx & (1 << 21), 0, "X2APIC masked");
        assert_eq!(leaf1.ecx & (1 << 24), 0, "TSC-deadline masked");
        // vPMU hidden: leaf 0xA all-zero.
        let leaf_a = m.entries.iter().find(|e| e.leaf == 0xA).expect("leaf 0xA");
        assert_eq!(
            (leaf_a.eax, leaf_a.ebx, leaf_a.ecx, leaf_a.edx),
            (0, 0, 0, 0)
        );
        // PV leaves hidden: the 0x4000_0000 block installs an all-zero entry.
        let pv = m
            .entries
            .iter()
            .find(|e| e.leaf == 0x4000_0000)
            .expect("PV leaf");
        assert_eq!((pv.eax, pv.ebx, pv.ecx, pv.edx), (0, 0, 0, 0));
    }

    #[test]
    fn resolve_cpuid_overlays_dynamic_cells() {
        // OSXSAVE follows CR4.OSXSAVE (bit 18).
        let base = CpuidEntry {
            leaf: 1,
            subleaf: 0,
            ecx: 0x76da_3203,
            ..Default::default()
        };
        assert_eq!(resolve_cpuid(base, 0, 0).ecx & (1 << 27), 0);
        assert_ne!(resolve_cpuid(base, 1 << 18, 0).ecx & (1 << 27), 0);
        // Level-echo: ECX[7:0] echoes the subleaf.
        let lvl = CpuidEntry {
            leaf: 0xB,
            subleaf: 5,
            ecx: 0x0000,
            ..Default::default()
        };
        assert_eq!(resolve_cpuid(lvl, 0, 0).ecx & 0xFF, 5);
        // XSAVE size follows XCR0 (AVX bit).
        let d0 = CpuidEntry {
            leaf: 0xD,
            subleaf: 0,
            ..Default::default()
        };
        assert_eq!(resolve_cpuid(d0, 0, 0x3).ebx, 0x240);
        assert_eq!(resolve_cpuid(d0, 0, 0x7).ebx, 0x340);
    }

    #[test]
    fn resolve_cpuid_bit_math_is_exact() {
        // Leaf 1 OSXSAVE overlay: `ecx = (ecx & !(1<<27)) | (CR4.OSXSAVE << 27)`.
        // A base with bits 27..31 set pins the *exact* clear-then-set bit math
        // (kills the `&`→`|`/`^`, the `!` deletion, and the `<<`→`>>` mutants —
        // the disjoint `|`/`^` cannot differ because the cleared field never holds
        // bit 27).
        let base = CpuidEntry {
            leaf: 1,
            subleaf: 0,
            ecx: 0xF800_0000,
            ..Default::default()
        };
        // CR4.OSXSAVE clear ⇒ bit 27 cleared, bits 28..31 preserved.
        assert_eq!(resolve_cpuid(base, 0, 0).ecx, 0xF000_0000);
        // CR4.OSXSAVE set (CR4 bit 18) ⇒ bit 27 set again.
        assert_eq!(resolve_cpuid(base, 1 << 18, 0).ecx, 0xF800_0000);

        // Leaf 0xB level-echo: `ecx = (subleaf & 0xFF) | (ecx & 0xFF00)` — the
        // low byte is the subleaf, byte 1 is preserved, the rest cleared. Exact
        // value kills the `&`→`|`/`^` mutants on both masks.
        let lvl = CpuidEntry {
            leaf: 0xB,
            subleaf: 5,
            ecx: 0xDEAD_1234,
            ..Default::default()
        };
        assert_eq!(resolve_cpuid(lvl, 0, 0).ecx, 0x0000_1205);

        // Leaf 0xD.0 XSAVE-area size: exactly 0x240 below AVX, 0x340 once XCR0
        // bit 2 (AVX) is enabled.
        let d0 = CpuidEntry {
            leaf: 0xD,
            subleaf: 0,
            ebx: 0xFFFF,
            ..Default::default()
        };
        assert_eq!(resolve_cpuid(d0, 0, 0x1).ebx, 0x240);
        assert_eq!(resolve_cpuid(d0, 0, 0x7).ebx, 0x340);
    }

    #[test]
    fn cpuid_model_subleaf_significance_is_exact() {
        // `subleaf_significant` for a `Subleaf::Single` row is `leaf_entry_count
        // > 1` — a single-subleaf leaf is insignificant, a multi-subleaf leaf is
        // significant. Pinning both kills the `>`→`==`/`<`/`>=` mutants.
        let m = cpuid_model();
        let leaf1 = m
            .entries
            .iter()
            .find(|e| e.leaf == 1 && e.subleaf == 0)
            .expect("leaf 1");
        assert!(
            !leaf1.subleaf_significant,
            "leaf 1 has a single subleaf ⇒ insignificant"
        );
        let leaf4: Vec<_> = m.entries.iter().filter(|e| e.leaf == 4).collect();
        assert!(leaf4.len() > 1, "leaf 4 has multiple subleaves");
        assert!(
            leaf4.iter().all(|e| e.subleaf_significant),
            "every leaf-4 subleaf is significant"
        );
    }

    // The §6 serialization tests build a 48 KiB canonical string from 1355 records
    // of pure, unsafe-free code — Miri would spend minutes on them with zero
    // UB-detection value, so they are `ignore`d there. The parse + per-index
    // dispatch map (the logic worth exercising) still run under Miri via the other
    // contract tests, which call `contract()` / `disp_map()`.
    #[test]
    #[cfg_attr(miri, ignore = "pure serialization; no unsafe — skip under Miri")]
    fn contract_hash_is_stable() {
        // Pure: two calls agree, and the hash is non-trivial.
        assert_eq!(contract_hash(), contract_hash());
        assert_ne!(contract_hash(), [0u8; 32]);
    }

    /// Gate-6 anti-drift assertion: `contract_hash()` must equal the hash the §6
    /// registry pins in `docs/cpu-msr-contract.toml` `[contract] contract_hash`.
    /// Live as of v3 (det-cfl-v1): the field is committed, so this gate is no
    /// longer `#[ignore]`d — computed-from-the-parsed-artifact must equal committed.
    /// Miri-ignored on the same grounds as its §6 siblings above (a ~97 s
    /// interpreted sha256 over the 48 KiB canonical form, pure unsafe-free code;
    /// task 98 / hm-d8o); the anti-drift gate itself runs on every native suite.
    #[test]
    #[cfg_attr(miri, ignore = "pure serialization; no unsafe — skip under Miri")]
    fn contract_hash_matches_committed_registry() {
        let computed: String = contract_hash().iter().map(|b| format!("{b:02x}")).collect();
        let committed = contract().contract_hash.clone();
        assert_eq!(
            committed.as_deref(),
            Some(computed.as_str()),
            "contract_hash() must equal the committed §6 registry hash. Pending: foreman commits \
             `contract_hash = \"{computed}\"` to docs/cpu-msr-contract.toml [contract]."
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "pure serialization; no unsafe — skip under Miri")]
    fn canonical_form_well_formed() {
        let form = canonical::serialize(contract());
        // Header anchors (literal §6 spelling).
        assert!(form.starts_with("contract-version=4\n"));
        assert!(form.contains("\nkernel-tag=v6.18.35\n"));
        assert!(form.contains("\ncpuid-baseline=det-cfl-v1\n"));
        assert!(form.contains("\nmxcsr-mask=0x0000ffff\n"));
        // Section anchors.
        assert!(form.contains(
            "\ncpuid 00000001.00000000 000906ec 00010800 dyn:osxsave:76da3203 0f8bbb7f\n"
        ));
        assert!(form.contains("\ncpuid-default zeroed\n"));
        assert!(
            form.contains(
                "\nmsr 00000010 emulate-vtime:vclock.tsc emulate-vtime:vclock.tsc.write\n"
            )
        );
        assert!(form.contains("\nmsr c0000080 allow-stateful allow-stateful\n"));
        assert!(form.contains("\nmmio-default allow-fixed:0000000000000000 deny-ignore-write\n"));
        assert!(form.contains("\nhost-assert family-model-stepping 06_9e_0c\n"));
        // §6 spelling: bracketed array, `, ` separator (not a bare `PKE,PKS`).
        assert!(form.contains("\nhost-assert cr4-force-reserved [PKE, PKS]\n"));
        assert!(form.contains("\nhost-assert host-absent HRESET\n"));
        // No trailing whitespace on any record line.
        for l in form.lines() {
            assert_eq!(l, l.trim_end(), "no trailing whitespace");
        }
    }

    /// **GOLDEN** §6 canonical form — the exact byte string the serializer must
    /// emit for the ratified v3 contract (det-cfl-v1), committed at
    /// `src/contract/testdata/canonical-v4.txt`. This locks **every** §6 spelling
    /// and ordering decision (header scalars, CPUID `dyn:` tokens, MSR formula ids,
    /// the timer device order, the 3-hex `xapic.<offset>` form, the 2-hex `cmos`
    /// tokens, and the bracketed `host-assert cr4-force-reserved [PKE, PKS]`), so
    /// **any** drift — including a parser change that alters a hashed value — is a
    /// failing byte diff. This is the gate that would have caught the
    /// `cr4-force-reserved` spelling bug; `contract_hash` is `sha256` of exactly
    /// these bytes, so a green golden ⇒ a correct hash.
    ///
    /// Regenerate **only** on a reviewed §6 change (and bump `contract-version`):
    /// write `canonical::serialize(contract())` to the golden file.
    #[test]
    #[cfg_attr(miri, ignore = "pure serialization; no unsafe — skip under Miri")]
    fn canonical_form_matches_golden() {
        let golden = include_str!("testdata/canonical-v4.txt");
        let form = canonical::serialize(contract());
        assert_eq!(
            form, golden,
            "§6 canonical form drifted from the committed golden \
             (src/contract/testdata/canonical-v4.txt). If this is an intended, reviewed §6 \
             change, bump contract-version and regenerate the golden file (contract::tests::regen_golden)."
        );
        // The committed v3 hash is sha256 of exactly the golden bytes.
        let hex: String = contract_hash().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "30839ae67142f265066be1051e93fcb4a1839c30bd3edd6d875ecdc1a37ddb67",
            "contract_hash must be sha256 of the golden canonical bytes"
        );
    }

    /// A small but multi-section synthetic contract for the formatting-invariance
    /// property below.
    const STABILITY_TOML: &str = "\
[contract]\n\
version = 3\n\
kernel-tag = \"v6.18.35\"\n\
cpuid-baseline = \"stab\"\n\
tsc-hz = 2000000000\n\
mxcsr-mask = \"0x0000ffff\"\n\
[[cpuid.entry]]\n\
leaf = \"0x1\"\n\
subleaf = \"0x0\"\n\
eax = \"0x50654\"\n\
ebx = \"0x10800\"\n\
ecx = \"dyn:osxsave:0x76da3203\"\n\
edx = \"0xf8bbb7f\"\n\
[[cpuid.entry]]\n\
leaf-lo = \"0x40000000\"\n\
leaf-hi = \"0x400000ff\"\n\
subleaf = \"*\"\n\
eax = \"0x0\"\n\
ebx = \"0x0\"\n\
ecx = \"0x0\"\n\
edx = \"0x0\"\n\
[[msr.entry]]\n\
index = \"0x10\"\n\
read = \"emulate-vtime\"\n\
read-param = \"vclock.tsc\"\n\
write = \"emulate-vtime\"\n\
write-param = \"vclock.tsc.write\"\n\
[[msr.entry]]\n\
index-lo = \"0x800\"\n\
index-hi = \"0x802\"\n\
read = \"deny-gp\"\n\
write = \"deny-gp\"\n\
[host-assert]\n\
rtm-disabled = true\n\
cr4-force-reserved = [\"PKE\", \"PKS\"]\n\
host-absent = [\"RDPID\", \"SHA\"]\n";

    /// Reconstruct `toml` with incidental, non-semantic formatting noise: leading
    /// indentation on every line, optional trailing `# comment`s, and extra blank
    /// lines — none of which the §6 form may depend on.
    fn inject_formatting_noise(toml: &str, comment_each: &[bool], leading_blanks: usize) -> String {
        let mut out = "\n".repeat(leading_blanks);
        for (i, line) in toml.lines().enumerate() {
            out.push_str("   "); // leading whitespace (the parser trims)
            out.push_str(line);
            if comment_each.get(i).copied().unwrap_or(false) {
                out.push_str("   # incidental comment");
            }
            out.push('\n');
            if comment_each.get(i).copied().unwrap_or(false) {
                out.push('\n'); // an extra blank line
            }
        }
        out
    }

    /// Miri-safe proptest config: fewer cases, and no failure persistence (its
    /// regression file needs `getcwd`, blocked by Miri isolation — see
    /// `tests/loader_proptest.rs`).
    fn pcfg(cases: u32) -> ProptestConfig {
        let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 4 } else { cases });
        if cfg!(miri) {
            cfg.failure_persistence = None;
        }
        cfg
    }

    proptest! {
        #![proptest_config(pcfg(48))]

        /// The canonical form is invariant to incidental input formatting — extra
        /// blank lines, trailing inline comments, surrounding whitespace — because
        /// the serializer derives only from the normative tables (sorted, fixed
        /// layout). This is the order/format independence the §6 `contract_hash`
        /// relies on: two artifacts that differ only in formatting hash identically.
        #[test]
        #[cfg_attr(miri, ignore = "pure serialization; no unsafe — skip under Miri")]
        fn prop_canonical_form_invariant_to_formatting(
            comment_each in proptest::collection::vec(any::<bool>(), 0..48),
            leading_blanks in 0usize..4,
        ) {
            let baseline = canonical::serialize(&Contract::parse(STABILITY_TOML));
            let noisy = inject_formatting_noise(STABILITY_TOML, &comment_each, leading_blanks);
            let got = canonical::serialize(&Contract::parse(&noisy));
            prop_assert_eq!(got, baseline);
        }
    }

    #[test]
    fn msr_index_set_is_disjoint_and_complete() {
        // The TOML header pins the union at 1043 indices, pairwise disjoint.
        let mut total = 0usize;
        for row in &contract().msr {
            total += row.index.indices().len();
        }
        assert_eq!(
            disp_map().len(),
            total,
            "MSR index sets are pairwise disjoint"
        );
        assert_eq!(total, 1043, "total MSR indices match the contract header");
    }

    // =======================================================================
    // AMD draft column (Deliverables 2–8) — the vendor axis on the one contract.
    // =======================================================================

    use super::parse::{ContractError, VendorId};

    /// SHA-256 of a contract's canonical form, as lowercase hex.
    fn hash_hex(c: &Contract) -> String {
        let mut hasher = Sha256::new();
        hasher.update(canonical::serialize(c).as_bytes());
        let out: [u8; 32] = hasher.finalize().into();
        out.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Intel byte-identity through the restructure (Deliverable 6): the live
    /// `contract()` is still the GenuineIntel column. Its canonical form and hash
    /// are pinned unchanged by `canonical_form_matches_golden` / the registry gate
    /// above (v4, `30839ae6…`); adding the `vendor` header key is zero-drift (the
    /// serializer never emits it), so those Intel gates stay green untouched.
    #[test]
    fn live_contract_is_the_intel_column() {
        assert_eq!(contract().vendor, VendorId::GenuineIntel);
        assert_eq!(contract().cpuid_baseline, "det-cfl-v1");
    }

    /// Draft-only guard (Deliverable 8) — **structural**: the AMD constructor is
    /// `#[cfg(test)]`, so no non-test build can name it; the live `contract()` path
    /// returns the Intel column, and the AMD draft carries the placeholder baseline.
    /// A live VM construction path cannot reach the AMD contract because the only
    /// symbol that returns it does not exist outside `cfg(test)`.
    #[test]
    fn amd_draft_is_unreachable_from_the_live_path() {
        // The live policy path is Intel.
        assert_eq!(contract().vendor, VendorId::GenuineIntel);
        // The AMD constructor (test-only) yields the AuthenticAMD draft column. The
        // two are distinct artifacts (distinct vendor + baseline); their hashes
        // differ too, pinned separately by the committed-hash gates below. Kept
        // parse-only here so the structural guard also runs under Miri.
        let amd = contract_amd_draft();
        assert_eq!(amd.vendor, VendorId::AuthenticAMD);
        assert_eq!(amd.cpuid_baseline, "det-zenN-v1");
        assert_ne!(contract().cpuid_baseline, amd.cpuid_baseline);
    }

    /// AMD round-trip + hash stability (Deliverable 8): the draft loads,
    /// canonicalizes, and produces a stable, non-trivial hash (two calls agree).
    #[test]
    #[cfg_attr(miri, ignore = "pure serialization; no unsafe — skip under Miri")]
    fn amd_draft_loads_and_canonicalizes() {
        let amd = contract_amd_draft();
        let a = hash_hex(amd);
        let b = hash_hex(amd);
        assert_eq!(a, b, "hash is a pure function of the parsed tables");
        assert_ne!(a, "0".repeat(64), "hash is non-trivial");
    }

    /// AMD computed hash == committed AMD `[contract] contract_hash` (Deliverable 8).
    #[test]
    #[cfg_attr(miri, ignore = "pure serialization; no unsafe — skip under Miri")]
    fn amd_contract_hash_matches_committed() {
        let amd = contract_amd_draft();
        let computed = hash_hex(amd);
        assert_eq!(
            amd.contract_hash.as_deref(),
            Some(computed.as_str()),
            "AMD draft contract_hash() must equal the committed hash in \
             docs/cpu-msr-contract-amd-draft.toml [contract]. Regenerate with \
             contract::tests::regen_amd_golden then commit `contract_hash = \"{computed}\"`."
        );
    }

    /// **GOLDEN** AMD canonical form — the exact bytes the serializer emits for the
    /// draft column, committed at `testdata/canonical-amd-draft.txt`. Locks every
    /// AMD spelling: the AuthenticAMD leaves, the `verified:on-silicon-pending-AE4`
    /// row qualifiers, the `applies-when:{legacy-perfmon,zen4+}` PMU markers, and the
    /// `transfer <section> …` markers. `contract_hash` is sha256 of exactly these bytes.
    #[test]
    #[cfg_attr(miri, ignore = "pure serialization; no unsafe — skip under Miri")]
    fn amd_canonical_form_matches_golden() {
        let golden = include_str!("testdata/canonical-amd-draft.txt");
        let form = canonical::serialize(contract_amd_draft());
        assert_eq!(
            form, golden,
            "AMD canonical form drifted from testdata/canonical-amd-draft.txt. If this is \
             an intended, reviewed change, bump [contract] version and regenerate the golden \
             (contract::tests::regen_amd_golden), then re-pin the committed hash."
        );
        // No trailing whitespace on any AMD record line (the transfer/qualifier
        // tokens append cleanly).
        for l in form.lines() {
            assert_eq!(l, l.trim_end(), "no trailing whitespace");
        }
    }

    /// AMD grammar anchors: the vendor axis is honest in the canonical form.
    #[test]
    #[cfg_attr(miri, ignore = "pure serialization; no unsafe — skip under Miri")]
    fn amd_canonical_form_well_formed() {
        let form = canonical::serialize(contract_amd_draft());
        // Header: the placeholder baseline, the deferred silicon scalars as 0.
        assert!(form.starts_with("contract-version=1\n"));
        assert!(form.contains("\ncpuid-baseline=det-zenN-v1\n"));
        assert!(
            form.contains("\ntsc-hz=0\n"),
            "silicon TSC freq deferred to AE-0"
        );
        // Leaf 0 AuthenticAMD vendor string, carrying the verify qualifier.
        assert!(form.contains(
            "\ncpuid 00000000.00000000 00000010 68747541 444d4163 69746e65 \
             verified:on-silicon-pending-AE4\n"
        ));
        // An allow-stateful AMD MSR + a deny-gp PMU MSR with its generation marker.
        assert!(form.contains(
            "\nmsr c0000080 allow-stateful allow-stateful verified:on-silicon-pending-AE4\n"
        ));
        assert!(form.contains(
            "\nmsr c0000300 deny-gp deny-gp verified:on-silicon-pending-AE4 applies-when:zen4+\n"
        ));
        // The PerfMonV2 global control/status set runs through GLOBAL_STATUS_SET.
        assert!(form.contains(
            "\nmsr c0000303 deny-gp deny-gp verified:on-silicon-pending-AE4 applies-when:zen4+\n"
        ));
        assert!(form.contains(
            "\nmsr c0010200 deny-gp deny-gp verified:on-silicon-pending-AE4 \
             applies-when:legacy-perfmon\n"
        ));
        // Section-level transfer markers replace the shared-ISA rows.
        assert!(form.contains("\ntransfer cpuid-standard unchanged-pending-AE4\n"));
        assert!(form.contains("\ntransfer msr-shared unchanged-pending-AE4\n"));
        assert!(form.contains("\ntransfer insn unchanged-pending-AE4\n"));
        assert!(form.contains("\ntransfer timer unchanged-pending-AE4\n"));
        assert!(form.contains("\ntransfer cmos unchanged-pending-AE4\n"));
        assert!(form.contains("\ntransfer mmio unchanged-pending-AE4\n"));
        assert!(form.contains("\ntransfer host-assert on-silicon-pending-AE4\n"));
    }

    /// Grammar validation: MSR index sets are pairwise disjoint **within** the AMD
    /// file (Deliverable 8 — the disjointness check generalized to the loaded vendor).
    #[test]
    fn amd_msr_index_set_is_disjoint() {
        let amd = contract_amd_draft();
        let mut seen = std::collections::BTreeSet::new();
        let mut total = 0usize;
        for row in &amd.msr {
            for idx in row.index.indices() {
                assert!(seen.insert(idx), "AMD MSR index {idx:#x} appears twice");
                total += 1;
            }
        }
        assert_eq!(
            seen.len(),
            total,
            "AMD MSR index sets are pairwise disjoint"
        );
        // Every materialized AMD MSR is in the AMD-distinct 0xc000/0xc001 space.
        assert!(
            seen.iter()
                .all(|&i| (0xc000_0000..=0xc001_ffff).contains(&i)),
            "materialized AMD MSRs live in the 0xc000_00xx / 0xc001_00xx space"
        );
    }

    /// `verify-on-silicon` coverage (Deliverable 8): every AMD **enforcement** row
    /// (every materialized CPUID/MSR row — the transfer sections are markers, not
    /// rows) carries the qualifier. A silently-trusted AMD row fails here.
    #[test]
    fn amd_every_enforcement_row_is_verify_on_silicon() {
        let amd = contract_amd_draft();
        for row in &amd.cpuid {
            assert_eq!(
                row.verified.as_deref(),
                Some("on-silicon-pending-AE4"),
                "AMD CPUID leaf {:#010x} lacks the verify-on-silicon marker",
                row.leaf.lo
            );
        }
        for row in &amd.msr {
            assert_eq!(
                row.verified.as_deref(),
                Some("on-silicon-pending-AE4"),
                "AMD MSR row {:?} lacks the verify-on-silicon marker",
                row.index.indices()
            );
        }
    }

    /// PerfMonV2-vs-legacy as a per-generation fact (Deliverable 4): the draft
    /// carries **both** PMU models as separate `applies-when`-marked sections, and
    /// the loader resolves **neither** — no single live PMU model is asserted.
    #[test]
    fn amd_carries_both_pmu_models_unresolved() {
        let amd = contract_amd_draft();
        let markers: std::collections::BTreeSet<&str> = amd
            .msr
            .iter()
            .filter_map(|r| r.applies_when.as_deref())
            .collect();
        assert!(
            markers.contains("legacy-perfmon"),
            "legacy PMU section present"
        );
        assert!(markers.contains("zen4+"), "PerfMonV2 section present");
        // Both live in the hashed draft data; the gate does NOT collapse them to one.
        assert_eq!(
            markers.len(),
            2,
            "exactly the two per-generation PMU models"
        );
    }

    /// Mixed-vendor refusal (Deliverable 8): the loader rejects a file whose `vendor`
    /// field disagrees with the axis it was loaded under, and an artifact whose
    /// declared vendor disagrees with its own CPUID leaf-0 vendor string.
    #[test]
    fn loader_refuses_vendor_axis_disagreement() {
        // AMD draft loaded under the Intel axis → VendorMismatch.
        assert_eq!(
            Contract::load(CONTRACT_AMD_DRAFT_TOML, VendorId::GenuineIntel).unwrap_err(),
            ContractError::VendorMismatch {
                expected: "GenuineIntel",
                found: "AuthenticAMD".to_string(),
            }
        );
        // Intel file loaded under the AMD axis → VendorMismatch.
        assert_eq!(
            Contract::load(CONTRACT_TOML, VendorId::AuthenticAMD).unwrap_err(),
            ContractError::VendorMismatch {
                expected: "AuthenticAMD",
                found: "GenuineIntel".to_string(),
            }
        );
        // Correct axes load cleanly.
        assert!(Contract::load(CONTRACT_TOML, VendorId::GenuineIntel).is_ok());
        assert!(Contract::load(CONTRACT_AMD_DRAFT_TOML, VendorId::AuthenticAMD).is_ok());
    }

    /// A mixed-vendor artifact: the `[contract] vendor` header claims AuthenticAMD,
    /// but CPUID leaf 0 spells the Intel vendor string — the structural guard fires.
    #[test]
    fn loader_refuses_mixed_vendor_artifact() {
        const MIXED: &str = "\
[contract]\n\
version = 1\n\
vendor = \"AuthenticAMD\"\n\
cpuid-baseline = \"det-zenN-v1\"\n\
[[cpuid.entry]]\n\
leaf = \"0x00000000\"\n\
subleaf = \"0x00000000\"\n\
eax = \"0x00000010\"\n\
ebx = \"0x756e6547\"\n\
ecx = \"0x6c65746e\"\n\
edx = \"0x49656e69\"\n\
verified = \"on-silicon-pending-AE4\"\n";
        let err = Contract::load(MIXED, VendorId::AuthenticAMD).unwrap_err();
        assert_eq!(
            err,
            ContractError::MixedVendor {
                declared: "AuthenticAMD",
                leaf0: "GenuineIntel".to_string(),
            }
        );
    }

    /// Fail-closed on a **present-but-invalid** vendor token: an unrecognized
    /// `[contract] vendor` string is refused (`UnknownVendor`), never silently
    /// defaulted to GenuineIntel. Only a genuinely *absent* key defaults.
    #[test]
    fn loader_refuses_present_but_invalid_vendor_token() {
        const BOGUS: &str = "\
[contract]\n\
version = 1\n\
vendor = \"NotARealVendor\"\n\
cpuid-baseline = \"whatever\"\n";
        // Refused under either axis — the token is invalid regardless of `expected`.
        for axis in [VendorId::GenuineIntel, VendorId::AuthenticAMD] {
            assert_eq!(
                Contract::load(BOGUS, axis).unwrap_err(),
                ContractError::UnknownVendor {
                    token: "NotARealVendor".to_string(),
                }
            );
        }
        // A genuinely absent vendor key still loads (legacy Intel fixtures).
        const NO_VENDOR: &str = "[contract]\nversion = 1\ncpuid-baseline = \"x\"\n";
        assert!(Contract::load(NO_VENDOR, VendorId::GenuineIntel).is_ok());
    }

    /// Fail-closed on a **present-but-malformed** leaf 0: a leaf-0 row using dynamic
    /// register rules, or non-UTF-8 constant bytes, cannot bypass the mixed-vendor
    /// guard by masquerading as an absent leaf 0 — it is refused (`MalformedLeaf0`).
    #[test]
    fn loader_refuses_malformed_leaf0() {
        // (a) leaf 0 with a dynamic register — not three frozen constants.
        const DYN_LEAF0: &str = "\
[contract]\n\
version = 1\n\
vendor = \"AuthenticAMD\"\n\
[[cpuid.entry]]\n\
leaf = \"0x00000000\"\n\
subleaf = \"0x00000000\"\n\
eax = \"0x00000010\"\n\
ebx = \"dyn:osxsave:0x0\"\n\
ecx = \"0x444d4163\"\n\
edx = \"0x69746e65\"\n\
verified = \"on-silicon-pending-AE4\"\n";
        assert_eq!(
            Contract::load(DYN_LEAF0, VendorId::AuthenticAMD).unwrap_err(),
            ContractError::MalformedLeaf0 {
                declared: "AuthenticAMD",
            }
        );

        // (b) leaf 0 whose constant bytes are not UTF-8 (0xffffffff registers).
        const NON_UTF8_LEAF0: &str = "\
[contract]\n\
version = 1\n\
vendor = \"AuthenticAMD\"\n\
[[cpuid.entry]]\n\
leaf = \"0x00000000\"\n\
subleaf = \"0x00000000\"\n\
eax = \"0x00000010\"\n\
ebx = \"0xffffffff\"\n\
ecx = \"0xffffffff\"\n\
edx = \"0xffffffff\"\n\
verified = \"on-silicon-pending-AE4\"\n";
        assert_eq!(
            Contract::load(NON_UTF8_LEAF0, VendorId::AuthenticAMD).unwrap_err(),
            ContractError::MalformedLeaf0 {
                declared: "AuthenticAMD",
            }
        );

        // A contract with NO leaf-0 row is still exempt (the guard is skipped).
        const NO_LEAF0: &str = "\
[contract]\n\
version = 1\n\
vendor = \"AuthenticAMD\"\n\
[[cpuid.entry]]\n\
leaf = \"0x80000000\"\n\
subleaf = \"0x00000000\"\n\
eax = \"0x80000008\"\n\
ebx = \"0x00000000\"\n\
ecx = \"0x00000000\"\n\
edx = \"0x00000000\"\n\
verified = \"on-silicon-pending-AE4\"\n";
        assert!(Contract::load(NO_LEAF0, VendorId::AuthenticAMD).is_ok());
    }

    /// A compact AMD-flavoured contract for the format-invariance property below —
    /// exercises the vendor axis, the `verified` / `applies-when` row qualifiers, and
    /// every `[transfers]` marker.
    const AMD_STABILITY_TOML: &str = "\
[contract]\n\
version = 1\n\
vendor = \"AuthenticAMD\"\n\
kernel-tag = \"v6.18.35\"\n\
cpuid-baseline = \"det-zenN-v1\"\n\
mxcsr-mask = \"0x0000ffff\"\n\
[[cpuid.entry]]\n\
leaf = \"0x00000000\"\n\
subleaf = \"0x00000000\"\n\
eax = \"0x00000010\"\n\
ebx = \"0x68747541\"\n\
ecx = \"0x444d4163\"\n\
edx = \"0x69746e65\"\n\
verified = \"on-silicon-pending-AE4\"\n\
[[cpuid.entry]]\n\
leaf = \"0x80000001\"\n\
subleaf = \"0x00000000\"\n\
eax = \"0x00000000\"\n\
ebx = \"0x00000000\"\n\
ecx = \"0x00000000\"\n\
edx = \"0x00000000\"\n\
verified = \"on-silicon-pending-AE4\"\n\
[[msr.entry]]\n\
index = \"0xc0000080\"\n\
read = \"allow-stateful\"\n\
write = \"allow-stateful\"\n\
verified = \"on-silicon-pending-AE4\"\n\
[[msr.entry]]\n\
index-lo = \"0xc0000300\"\n\
index-hi = \"0xc0000302\"\n\
read = \"deny-gp\"\n\
write = \"deny-gp\"\n\
verified = \"on-silicon-pending-AE4\"\n\
applies-when = \"zen4+\"\n\
[transfers]\n\
cpuid-standard = \"unchanged-pending-AE4\"\n\
msr-shared = \"unchanged-pending-AE4\"\n\
insn = \"unchanged-pending-AE4\"\n\
timer = \"unchanged-pending-AE4\"\n\
cmos = \"unchanged-pending-AE4\"\n\
mmio = \"unchanged-pending-AE4\"\n\
host-assert = \"on-silicon-pending-AE4\"\n";

    proptest! {
        // ≥256 native cases per the AMD-draft format-invariance gate (the small
        // ~4 KiB AMD form keeps this well under the test-runtime budget).
        #![proptest_config(pcfg(256))]

        /// Format-invariance for the AMD column (Deliverable 8): incidental input
        /// formatting — leading whitespace, trailing comments, blank lines — never
        /// changes the canonical form / hash, exactly as for the Intel column.
        #[test]
        #[cfg_attr(miri, ignore = "pure serialization; no unsafe — skip under Miri")]
        fn prop_amd_canonical_form_invariant_to_formatting(
            comment_each in proptest::collection::vec(any::<bool>(), 0..48),
            leading_blanks in 0usize..4,
        ) {
            let baseline = canonical::serialize(&Contract::parse(AMD_STABILITY_TOML));
            let noisy = inject_formatting_noise(AMD_STABILITY_TOML, &comment_each, leading_blanks);
            let got = canonical::serialize(&Contract::parse(&noisy));
            prop_assert_eq!(got, baseline);
        }
    }

    /// Prints the computed §6 canonical form size + the current `contract_hash` so the
    /// foreman can commit it to `docs/cpu-msr-contract.toml`. Run with:
    /// `cargo test -p vmm-core contract::tests::report_contract_hash -- --nocapture`.
    #[test]
    #[cfg_attr(miri, ignore = "pure serialization; no unsafe — skip under Miri")]
    fn report_contract_hash() {
        let form = canonical::serialize(contract());
        let hash = contract_hash();
        let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
        eprintln!("=== contract_hash (v{}) ===", contract().version);
        eprintln!("canonical-form bytes: {}", form.len());
        eprintln!("canonical-form lines: {}", form.lines().count());
        eprintln!("contract_hash = {hex}");
    }

    /// Regenerate the committed golden canonical form. **Ignored** so it never runs
    /// in the normal suite (it writes a source file); run deliberately on a reviewed
    /// §6 change after bumping `contract-version`:
    /// `cargo test -p vmm-core contract::tests::regen_golden -- --ignored`.
    /// Then update `canonical_form_matches_golden`'s expected hash to the new value.
    #[test]
    #[ignore = "writes src/vendor/x86/contract/testdata/canonical-v4.txt; run manually on a reviewed §6 bump"]
    fn regen_golden() {
        let form = canonical::serialize(contract());
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/vendor/x86/contract/testdata/canonical-v4.txt"
        );
        std::fs::write(path, &form).expect("write golden");
    }

    /// Regenerate the committed **AMD** golden + report its hash. **Ignored** so it
    /// never runs in the normal suite. Run deliberately on a reviewed AMD-column
    /// change (e.g. AE-0 pinning `det-zenN-v1`) after bumping the AMD `[contract]
    /// version`, then commit the new `contract_hash` to
    /// `docs/cpu-msr-contract-amd-draft.toml`:
    /// `cargo test -p vmm-core contract::tests::regen_amd_golden -- --ignored --nocapture`.
    #[test]
    #[ignore = "writes testdata/canonical-amd-draft.txt; run manually on a reviewed AMD-column change"]
    fn regen_amd_golden() {
        let amd = contract_amd_draft();
        let form = canonical::serialize(amd);
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/vendor/x86/contract/testdata/canonical-amd-draft.txt"
        );
        std::fs::write(path, &form).expect("write AMD golden");
        eprintln!("=== AMD draft contract_hash ===");
        eprintln!("canonical-form bytes: {}", form.len());
        eprintln!("contract_hash = {}", hash_hex(amd));
    }
}
