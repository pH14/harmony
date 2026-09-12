// SPDX-License-Identifier: AGPL-3.0-or-later
//! CPUID model and MSR-filter policy built from the checked-in x86 contract in
//! `consonance/vmm-core/contracts/x86/guest.toml`.
//!
//! `vmm-core` owns the policy; the install *mechanism* (`KVM_SET_CPUID2`,
//! `KVM_X86_SET_MSR_FILTER`, `KVM_CAP_X86_USER_SPACE_MSR`) is KVM-specific and
//! lives **below the trait** in `vmm-backend`. These functions produce
//! backend-agnostic values ([`vmm_backend::CpuidModel`] / [`vmm_backend::MsrFilter`]
//! / [`MsrDisposition`]) that the Linux composition root hands to the backend
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

use crate::virtual_time::VirtualTimeTiming;

mod canonical;
mod parse;

use parse::{Contract, Subleaf, VendorId};

/// The shared x86 guest policy, embedded at compile time.
const CONTRACT_TOML: &str = include_str!("../../../../contracts/x86/guest.toml");

/// The shared guest policy, built once on first use.
/// The declared guest vendor must agree with the CPUID leaf-0 string. This
/// validates embedded policy bytes; it does not inspect the physical host.
fn contract() -> &'static Contract {
    static CACHE: OnceLock<Contract> = OnceLock::new();
    CACHE.get_or_init(|| {
        Contract::load(CONTRACT_TOML, VendorId::GenuineIntel)
            .expect("embedded guest policy must declare vendor = \"GenuineIntel\"")
    })
}

/// `KVM_MSR_EXIT_REASON_FILTER` bit value (bit 0). Written `1` rather than `1 << 0`
/// so the shift operator carries no equivalent (`1 << 0` ≡ `1 >> 0`) mutant.
pub const MSR_EXIT_REASON_FILTER: u64 = 1;
/// `KVM_MSR_EXIT_REASON_UNKNOWN` bit value.
pub const MSR_EXIT_REASON_UNKNOWN: u64 = 1 << 1;
/// `KVM_MSR_EXIT_REASON_INVAL` bit value.
pub const MSR_EXIT_REASON_INVAL: u64 = 1 << 2;

/// The mask `vmm-backend` must enable on `KVM_CAP_X86_USER_SPACE_MSR` **before
/// installing the MSR filter** (x86 CPU contract; api.rst §4.97 ordering):
/// `FILTER | UNKNOWN | INVAL`. Enabling the cap first is load-bearing — otherwise
/// a denied/unknown/invalid MSR becomes a silent in-kernel `#GP` instead of a loud
/// `KVM_EXIT_X86_RDMSR/WRMSR`.
pub const USER_SPACE_MSR_MASK: u64 =
    MSR_EXIT_REASON_FILTER | MSR_EXIT_REASON_UNKNOWN | MSR_EXIT_REASON_INVAL;

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
    /// `emulate-vtime` rows (x86 CPU contract): `MSR_IA32_TSC` (0x10) and
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
        _ => MsrDisposition::DenyGp,
    }
}

/// Parse a `"0x...."`/bare-hex 64-bit param.
fn hex64(s: &str) -> u64 {
    u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).unwrap_or(0)
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

/// The normative x86 virtual_time timing row set, read from the ratified
/// contract's `vtime-*` header records and covered by `contract_hash`.
/// Production composition never uses `VirtualTimeTiming::default()`'s M0
/// placeholders.
///
/// Classes: interrupt-controller = the xAPIC MMIO page, the 8259 PIC data
/// ports, and the ELCR ports; serial = the 8250 UART ports; paravirtual = any
/// other modeled platform device (the report channel, the accepted legacy
/// ISA/PCI ports); time read = the `emulate-vtime` TSC MSRs and RDTSC/RDTSCP;
/// architectural control = every other surfaced MSR/CPUID/RDRAND/RDSEED trap;
/// execution tick = the guest kernel's ring on `VIRTUAL_TIME_TICK_PORT`.
pub fn virtual_time_timing() -> VirtualTimeTiming {
    let c = contract();
    let vns = |value: i64, record: &str| {
        u64::try_from(value).unwrap_or_else(|_| panic!("contract {record} must be non-negative"))
    };
    let timing = VirtualTimeTiming {
        interrupt_controller_mmio_vns: vns(
            c.vtime_interrupt_controller_vns,
            "vtime-interrupt-controller-vns",
        ),
        serial_mmio_vns: vns(c.vtime_serial_vns, "vtime-serial-vns"),
        paravirtual_device_mmio_vns: vns(c.vtime_paravirtual_vns, "vtime-paravirtual-vns"),
        trapped_time_read_vns: vns(c.vtime_time_read_vns, "vtime-time-read-vns"),
        architectural_control_vns: vns(c.vtime_arch_control_vns, "vtime-arch-control-vns"),
        execution_tick_vns: vns(c.vtime_execution_tick_vns, "vtime-execution-tick-vns"),
    };
    assert!(
        timing.execution_tick_vns < clockevent_period_vns(),
        "vtime-execution-tick-vns must stay strictly below vtime-clockevent-period-vns"
    );
    timing
}

/// The guest's clockevent period from the contract's `vtime-clockevent-period-vns`
/// header record. It is the seventh shared timing row: no exit is charged with it,
/// so it is not a [`VirtualTimeTiming`] field, but it bounds the execution tick and
/// both architectures must carry the same value.
pub(crate) fn clockevent_period_vns() -> u64 {
    u64::try_from(contract().vtime_clockevent_period_vns)
        .unwrap_or_else(|_| panic!("contract vtime-clockevent-period-vns must be non-negative"))
}

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
/// userspace CPUID emulation stays contract-correct. Pure.
pub fn resolve_cpuid(base: CpuidEntry, cr4: u64, xcr0: u64) -> CpuidEntry {
    let mut e = base;
    match (base.leaf, base.subleaf) {
        (0x1, 0) => {
            let osxsave = ((cr4 >> 18) & 1) as u32;
            e.ecx = (e.ecx & !(1 << 27)) | (osxsave << 27);
        }
        (0xB | 0x1F, _) => {
            e.ecx = (base.subleaf & 0xFF) | (e.ecx & 0xFF00);
        }
        (0xD, 0) => {
            e.ebx = if xcr0 & 0x4 != 0 { 0x340 } else { 0x240 };
        }
        _ => {}
    }
    e
}

/// The MSR-filter allow set: exactly the `allow-stateful` rows — the only MSRs
/// KVM keeps servicing in-kernel — as [`vmm_backend::MsrFilter`] so it feeds
/// straight into [`vmm_backend::Backend::set_msr_filter`]. Every other disposition
/// is left out on purpose so the access surfaces to a userspace exit. Ranges are
/// canonical, sorted, and non-overlapping; the backend installs them under
/// `KVM_MSR_FILTER_DEFAULT_DENY` with both READ and WRITE flags (well within
/// KVM's 16-ranges-per-direction limit).
pub fn msr_filter_allow() -> MsrFilter {
    let mut indices: Vec<u32> = Vec::new();
    for row in &contract().msr {
        if row.read == "allow-stateful" && row.write == "allow-stateful" {
            indices.extend(row.index.indices());
        }
    }
    indices.sort_unstable();
    indices.dedup();

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

/// SHA-256 of the canonical serialized contract this policy was built from (§6
/// `contract_hash`). The bytes are the §6 canonical form emitted by
/// [`canonical::serialize`] from the same parsed tables the runtime policy uses,
/// so policy can never drift from the ratified contract.
///
/// The guest-policy hash is committed in `consonance/vmm-core/contracts/x86/guest.toml`
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
        assert!(f.allow_inkernel.len() <= 16);
        for w in f.allow_inkernel.windows(2) {
            assert!(
                w[0].base + w[0].count <= w[1].base,
                "ranges sorted/disjoint"
            );
        }
        let covered = |idx: u32| f.allow_indices().any(|i| i == idx);
        assert!(covered(0xC000_0080), "EFER in-kernel");
        assert!(covered(0x277), "CR_PAT in-kernel");
        assert!(!covered(0x17), "PLATFORM_ID not in-kernel");
        assert!(!covered(0x10), "IA32_TSC not in-kernel");
    }

    #[test]
    fn msr_dispositions_match_contract() {
        assert_eq!(rdmsr_disposition(0xDEAD_BEEF), MsrDisposition::DenyGp);
        assert_eq!(rdmsr_disposition(0x17), MsrDisposition::AllowFixed(0));
        assert_eq!(wrmsr_disposition(0x17, 0), MsrDisposition::DenyGp);
        assert_eq!(
            rdmsr_disposition(0x1B),
            MsrDisposition::AllowFixed(0xFEE0_0900)
        );
        assert_eq!(wrmsr_disposition(0x1B, 0), MsrDisposition::DenyIgnoreWrite);
        for idx in [0x10u32, 0x3b] {
            assert_eq!(rdmsr_disposition(idx), MsrDisposition::EmulateVtime);
            assert_eq!(wrmsr_disposition(idx, 0), MsrDisposition::EmulateVtime);
        }
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
        assert_eq!(leaf1.ecx & (1 << 21), 0, "X2APIC masked");
        assert_eq!(leaf1.ecx & (1 << 24), 0, "TSC-deadline masked");
        let leaf_a = m.entries.iter().find(|e| e.leaf == 0xA).expect("leaf 0xA");
        assert_eq!(
            (leaf_a.eax, leaf_a.ebx, leaf_a.ecx, leaf_a.edx),
            (0, 0, 0, 0)
        );
        let pv = m
            .entries
            .iter()
            .find(|e| e.leaf == 0x4000_0000)
            .expect("PV leaf");
        assert_eq!((pv.eax, pv.ebx, pv.ecx, pv.edx), (0, 0, 0, 0));
    }

    #[test]
    fn cpuid_model_hides_hardware_rng() {
        let model = cpuid_model();
        assert_eq!(
            model.entries.iter().find(|e| e.leaf == 1).unwrap().ecx & (1 << 30),
            0
        );
        assert_eq!(
            model
                .entries
                .iter()
                .find(|e| e.leaf == 7 && e.subleaf == 0)
                .unwrap()
                .ebx
                & (1 << 18),
            0
        );
    }

    #[test]
    fn resolve_cpuid_overlays_dynamic_cells() {
        let base = CpuidEntry {
            leaf: 1,
            subleaf: 0,
            ecx: 0x76da_3203,
            ..Default::default()
        };
        assert_eq!(resolve_cpuid(base, 0, 0).ecx & (1 << 27), 0);
        assert_ne!(resolve_cpuid(base, 1 << 18, 0).ecx & (1 << 27), 0);
        let lvl = CpuidEntry {
            leaf: 0xB,
            subleaf: 5,
            ecx: 0x0000,
            ..Default::default()
        };
        assert_eq!(resolve_cpuid(lvl, 0, 0).ecx & 0xFF, 5);
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
        let base = CpuidEntry {
            leaf: 1,
            subleaf: 0,
            ecx: 0xF800_0000,
            ..Default::default()
        };
        assert_eq!(resolve_cpuid(base, 0, 0).ecx, 0xF000_0000);
        assert_eq!(resolve_cpuid(base, 1 << 18, 0).ecx, 0xF800_0000);

        let lvl = CpuidEntry {
            leaf: 0xB,
            subleaf: 5,
            ecx: 0xDEAD_1234,
            ..Default::default()
        };
        assert_eq!(resolve_cpuid(lvl, 0, 0).ecx, 0x0000_1205);

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

    #[test]
    #[cfg_attr(miri, ignore = "pure serialization; no unsafe — skip under Miri")]
    fn contract_hash_is_stable() {
        assert_eq!(contract_hash(), contract_hash());
        assert_ne!(contract_hash(), [0u8; 32]);
    }

    /// Gate-6 anti-drift assertion: `contract_hash()` must equal the hash the §6
    /// registry pins in `contracts/x86/guest.toml` `[contract] contract_hash`.
    /// The computed hash must equal the committed guest-policy identity.
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
            "contract_hash() must equal the committed registry hash. Update \
             `contract_hash = \"{computed}\"` in contracts/x86/guest.toml."
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "pure serialization; no unsafe — skip under Miri")]
    fn canonical_form_well_formed() {
        let form = canonical::serialize(contract());
        assert!(form.starts_with("contract-version=6\n"));
        assert!(form.contains("\nkernel-tag=v6.18.35\n"));
        assert!(form.contains("\ncpuid-baseline=harmony-x86-v1\n"));
        assert!(form.contains("\nmxcsr-mask=0x0000ffff\n"));
        assert!(form.contains(concat!(
            "\nvtime-interrupt-controller-vns=10000\n",
            "vtime-serial-vns=10000\n",
            "vtime-paravirtual-vns=10000\n",
            "vtime-time-read-vns=1\n",
            "vtime-arch-control-vns=10000\n",
            "vtime-execution-tick-vns=100000\n",
            "vtime-clockevent-period-vns=10000000\n",
        )));
        assert!(form.contains(
            "\ncpuid 00000001.00000000 000906ec 00010800 dyn:osxsave:36da3203 0f8bbb7f\n"
        ));
        assert!(form.contains("\ncpuid-default zeroed\n"));
        assert!(
            form.contains(
                "\nmsr 00000010 emulate-vtime:vclock.tsc emulate-vtime:vclock.tsc.write\n"
            )
        );
        assert!(form.contains("\nmsr c0000080 allow-stateful allow-stateful\n"));
        assert!(form.contains("\nmmio-default allow-fixed:0000000000000000 deny-ignore-write\n"));
        assert!(!form.contains("host-assert"));
        assert!(form.contains("\nguest cr4-force-reserved [PKE, PKS]\n"));
        assert!(form.contains("\nguest ucode-rev 0x0000000100000000\n"));
        assert!(!form.contains("fault-absent"));
        for l in form.lines() {
            assert_eq!(l, l.trim_end(), "no trailing whitespace");
        }
    }

    /// **GOLDEN** §6 canonical form — the exact byte string the serializer must
    /// emit for the version 6 guest policy, committed at
    /// `src/vendor/x86/contract/testdata/canonical-v6.txt`. This locks **every** §6 spelling
    /// and ordering decision (header scalars, CPUID `dyn:` tokens, MSR formula ids,
    /// the timer device order, the 3-hex `xapic.<offset>` form, the 2-hex `cmos`
    /// tokens, and the bracketed `guest cr4-force-reserved [PKE, PKS]`), so
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
        let golden = include_str!("testdata/canonical-v6.txt");
        let form = canonical::serialize(contract());
        assert_eq!(
            form, golden,
            "§6 canonical form drifted from the committed golden \
             (src/vendor/x86/contract/testdata/canonical-v6.txt). If this is an intended, reviewed §6 \
             change, bump contract-version and regenerate the golden file (contract::tests::regen_golden)."
        );
        let hex: String = contract_hash().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex, "e2cf2a502d598e042684a3bd5807aec0095f4fc9f7ee3d4eb538d3372c4a141d",
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
[guest]\n\
cr4-force-reserved = [\"PKE\", \"PKS\"]\n\
ucode-rev = \"0x0000000100000000\"\n";

    /// Reconstruct `toml` with incidental, non-semantic formatting noise: leading
    /// indentation on every line, optional trailing `# comment`s, and extra blank
    /// lines — none of which the §6 form may depend on.
    fn inject_formatting_noise(toml: &str, comment_each: &[bool], leading_blanks: usize) -> String {
        let mut out = "\n".repeat(leading_blanks);
        for (i, line) in toml.lines().enumerate() {
            out.push_str("   ");
            out.push_str(line);
            if comment_each.get(i).copied().unwrap_or(false) {
                out.push_str("   # incidental comment");
            }
            out.push('\n');
            if comment_each.get(i).copied().unwrap_or(false) {
                out.push('\n');
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

    use super::parse::{ContractError, VendorId};

    /// Mixed-vendor refusal (Deliverable 8): the loader rejects a file whose `vendor`
    /// field disagrees with the axis it was loaded under, and an artifact whose
    /// declared vendor disagrees with its own CPUID leaf-0 vendor string.
    #[test]
    fn loader_refuses_vendor_axis_disagreement() {
        assert_eq!(
            Contract::load(CONTRACT_TOML, VendorId::AuthenticAMD).unwrap_err(),
            ContractError::VendorMismatch {
                expected: "AuthenticAMD",
                found: "GenuineIntel".to_string(),
            }
        );
        assert!(Contract::load(CONTRACT_TOML, VendorId::GenuineIntel).is_ok());
    }

    /// A mixed-vendor artifact: the `[contract] vendor` header claims AuthenticAMD,
    /// but CPUID leaf 0 spells the Intel vendor string — the structural guard fires.
    #[test]
    fn loader_refuses_mixed_vendor_artifact() {
        const MIXED: &str = "\
[contract]\n\
version = 1\n\
vendor = \"AuthenticAMD\"\n\
cpuid-baseline = \"test-guest\"\n\
[[cpuid.entry]]\n\
leaf = \"0x00000000\"\n\
subleaf = \"0x00000000\"\n\
eax = \"0x00000010\"\n\
ebx = \"0x756e6547\"\n\
ecx = \"0x6c65746e\"\n\
edx = \"0x49656e69\"\n\
";
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
        for axis in [VendorId::GenuineIntel, VendorId::AuthenticAMD] {
            assert_eq!(
                Contract::load(BOGUS, axis).unwrap_err(),
                ContractError::UnknownVendor {
                    token: "NotARealVendor".to_string(),
                }
            );
        }
        const NO_VENDOR: &str = "[contract]\nversion = 1\ncpuid-baseline = \"x\"\n";
        assert!(Contract::load(NO_VENDOR, VendorId::GenuineIntel).is_ok());
    }

    /// Fail-closed on a **present-but-malformed** leaf 0: a leaf-0 row using dynamic
    /// register rules, or non-UTF-8 constant bytes, cannot bypass the mixed-vendor
    /// guard by masquerading as an absent leaf 0 — it is refused (`MalformedLeaf0`).
    #[test]
    fn loader_refuses_malformed_leaf0() {
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
";
        assert_eq!(
            Contract::load(DYN_LEAF0, VendorId::AuthenticAMD).unwrap_err(),
            ContractError::MalformedLeaf0 {
                declared: "AuthenticAMD",
            }
        );

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
";
        assert_eq!(
            Contract::load(NON_UTF8_LEAF0, VendorId::AuthenticAMD).unwrap_err(),
            ContractError::MalformedLeaf0 {
                declared: "AuthenticAMD",
            }
        );

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
";
        assert!(Contract::load(NO_LEAF0, VendorId::AuthenticAMD).is_ok());
    }

    /// Positive validation of the leaf-0 shape (round-4 REDESIGN): the guard no longer
    /// enumerates malformed shapes (which lost three rounds) — it accepts **only** the
    /// one canonical shape (exactly one all-constant single `(0,0)` row spelling the
    /// declared vendor) and refuses everything else. A range-form covering row is now
    /// `MalformedLeaf0` **regardless of the vendor bytes** — a range is not the
    /// canonical single-leaf shape, so it can neither smuggle a foreign vendor nor
    /// slip through by spelling the right one.
    #[test]
    fn loader_refuses_noncanonical_leaf0_shapes() {
        const RANGE_INTEL: &str = "\
[contract]\n\
version = 1\n\
vendor = \"AuthenticAMD\"\n\
[[cpuid.entry]]\n\
leaf-lo = \"0x00000000\"\n\
leaf-hi = \"0x00000005\"\n\
subleaf = \"*\"\n\
eax = \"0x00000010\"\n\
ebx = \"0x756e6547\"\n\
ecx = \"0x6c65746e\"\n\
edx = \"0x49656e69\"\n\
";
        assert_eq!(
            Contract::load(RANGE_INTEL, VendorId::AuthenticAMD).unwrap_err(),
            ContractError::MalformedLeaf0 {
                declared: "AuthenticAMD",
            }
        );

        const RANGE_AMD: &str = "\
[contract]\n\
version = 1\n\
vendor = \"AuthenticAMD\"\n\
[[cpuid.entry]]\n\
leaf-lo = \"0x00000000\"\n\
leaf-hi = \"0x00000005\"\n\
subleaf = \"*\"\n\
eax = \"0x00000010\"\n\
ebx = \"0x68747541\"\n\
ecx = \"0x444d4163\"\n\
edx = \"0x69746e65\"\n\
";
        assert_eq!(
            Contract::load(RANGE_AMD, VendorId::AuthenticAMD).unwrap_err(),
            ContractError::MalformedLeaf0 {
                declared: "AuthenticAMD",
            }
        );

        const DYN_EAX: &str = "\
[contract]\n\
version = 1\n\
vendor = \"AuthenticAMD\"\n\
[[cpuid.entry]]\n\
leaf = \"0x00000000\"\n\
subleaf = \"0x00000000\"\n\
eax = \"dyn:osxsave:0x10\"\n\
ebx = \"0x68747541\"\n\
ecx = \"0x444d4163\"\n\
edx = \"0x69746e65\"\n\
";
        assert_eq!(
            Contract::load(DYN_EAX, VendorId::AuthenticAMD).unwrap_err(),
            ContractError::MalformedLeaf0 {
                declared: "AuthenticAMD",
            }
        );

        const TWO_COVERING: &str = "\
[contract]\n\
version = 1\n\
vendor = \"AuthenticAMD\"\n\
[[cpuid.entry]]\n\
leaf = \"0x00000000\"\n\
subleaf = \"0x00000000\"\n\
eax = \"0x00000010\"\n\
ebx = \"0x68747541\"\n\
ecx = \"0x444d4163\"\n\
edx = \"0x69746e65\"\n\
[[cpuid.entry]]\n\
leaf = \"0x00000000\"\n\
subleaf = \"*\"\n\
eax = \"0x00000010\"\n\
ebx = \"0x68747541\"\n\
ecx = \"0x444d4163\"\n\
edx = \"0x69746e65\"\n\
";
        assert_eq!(
            Contract::load(TWO_COVERING, VendorId::AuthenticAMD).unwrap_err(),
            ContractError::MalformedLeaf0 {
                declared: "AuthenticAMD",
            }
        );

        const GOOD: &str = "\
[contract]\n\
version = 1\n\
vendor = \"AuthenticAMD\"\n\
[[cpuid.entry]]\n\
leaf = \"0x00000000\"\n\
subleaf = \"0x00000000\"\n\
eax = \"0x00000010\"\n\
ebx = \"0x68747541\"\n\
ecx = \"0x444d4163\"\n\
edx = \"0x69746e65\"\n\
";
        assert!(Contract::load(GOOD, VendorId::AuthenticAMD).is_ok());

        const GOOD_WRONG_VENDOR: &str = "\
[contract]\n\
version = 1\n\
vendor = \"AuthenticAMD\"\n\
[[cpuid.entry]]\n\
leaf = \"0x00000000\"\n\
subleaf = \"0x00000000\"\n\
eax = \"0x00000010\"\n\
ebx = \"0x756e6547\"\n\
ecx = \"0x6c65746e\"\n\
edx = \"0x49656e69\"\n\
";
        assert_eq!(
            Contract::load(GOOD_WRONG_VENDOR, VendorId::AuthenticAMD).unwrap_err(),
            ContractError::MixedVendor {
                declared: "AuthenticAMD",
                leaf0: "GenuineIntel".to_string(),
            }
        );

        const NONZERO_SUBLEAF: &str = "\
[contract]\n\
version = 1\n\
vendor = \"AuthenticAMD\"\n\
[[cpuid.entry]]\n\
leaf = \"0x00000000\"\n\
subleaf = \"0x00000001\"\n\
eax = \"0x00000000\"\n\
ebx = \"0x756e6547\"\n\
ecx = \"0x6c65746e\"\n\
edx = \"0x49656e69\"\n\
";
        assert!(Contract::load(NONZERO_SUBLEAF, VendorId::AuthenticAMD).is_ok());
    }

    /// Prints the computed §6 canonical form size + the current `contract_hash` so the
    /// maintainer can commit it to `contracts/x86/guest.toml`. Run with:
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
    #[ignore = "writes src/vendor/x86/contract/testdata/canonical-v6.txt; run manually on a reviewed §6 bump"]
    fn regen_golden() {
        let form = canonical::serialize(contract());
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/vendor/x86/contract/testdata/canonical-v6.txt"
        );
        std::fs::write(path, &form).expect("write golden");
    }
}
