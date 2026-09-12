// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};
use vmm_backend::{CpuidEntry, CpuidModel, MsrFilter, MsrRange};

use crate::virtual_time::VirtualTimeTiming;

mod canonical;
mod parse;

use parse::{Contract, Subleaf, VendorId};

const CONTRACT_TOML: &str = include_str!("../../../../contracts/x86/guest.toml");

fn contract() -> &'static Contract {
    static CACHE: OnceLock<Contract> = OnceLock::new();
    CACHE.get_or_init(|| {
        Contract::load(CONTRACT_TOML, VendorId::GenuineIntel)
            .expect("embedded guest policy must declare vendor = \"GenuineIntel\"")
    })
}

pub const MSR_EXIT_REASON_FILTER: u64 = 1;
pub const MSR_EXIT_REASON_UNKNOWN: u64 = 1 << 1;
pub const MSR_EXIT_REASON_INVAL: u64 = 1 << 2;

pub const USER_SPACE_MSR_MASK: u64 =
    MSR_EXIT_REASON_FILTER | MSR_EXIT_REASON_UNKNOWN | MSR_EXIT_REASON_INVAL;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MsrDisposition {
    AllowStateful,
    AllowFixed(u64),
    EmulateVtime,
    DenyGp,
    DenyIgnoreWrite,
}

fn disposition_of(token: &str, param: Option<&str>) -> MsrDisposition {
    match token {
        "allow-stateful" => MsrDisposition::AllowStateful,
        "allow-fixed" => MsrDisposition::AllowFixed(param.map(hex64).unwrap_or(0)),
        "emulate-vtime" => MsrDisposition::EmulateVtime,
        "deny-ignore-write" => MsrDisposition::DenyIgnoreWrite,
        _ => MsrDisposition::DenyGp,
    }
}

fn hex64(s: &str) -> u64 {
    u64::from_str_radix(s.trim().trim_start_matches("0x"), 16).unwrap_or(0)
}

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

pub fn rdmsr_disposition(index: u32) -> MsrDisposition {
    disp_map()
        .get(&index)
        .map_or(MsrDisposition::DenyGp, |(r, _)| *r)
}

pub fn wrmsr_disposition(index: u32, value: u64) -> MsrDisposition {
    let _ = value;
    disp_map()
        .get(&index)
        .map_or(MsrDisposition::DenyGp, |(_, w)| *w)
}

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

pub(crate) fn clockevent_period_vns() -> u64 {
    u64::try_from(contract().vtime_clockevent_period_vns)
        .unwrap_or_else(|_| panic!("contract vtime-clockevent-period-vns must be non-negative"))
}

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

pub fn contract_hash() -> [u8; 32] {
    let canonical = canonical::serialize(contract());
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {

    #[test]
    fn msr_exit_reason_bits_are_pinned() {
        assert_eq!(MSR_EXIT_REASON_UNKNOWN, 0x2);
        assert_eq!(MSR_EXIT_REASON_INVAL, 0x4);
    }
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

    fn pcfg(cases: u32) -> ProptestConfig {
        let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 4 } else { cases });
        if cfg!(miri) {
            cfg.failure_persistence = None;
        }
        cfg
    }

    proptest! {
        #![proptest_config(pcfg(48))]

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
