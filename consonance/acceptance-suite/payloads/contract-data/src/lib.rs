//! Conformance tables for the instruction-sweep payloads (task 18), generated
//! at build time from `docs/cpu-msr-contract.toml` so every expected value
//! traces to the committed contract — a contract bump surfaces as a payload /
//! golden diff rather than silent drift. Shared verbatim between the bare-metal
//! sweep payloads (`no_std`) and host-side tests, like `compute-core`.
//!
//! Normativity note: the authoritative source is the TOML (the hashed §6
//! surface). `docs/fragments/cpuid-model.md` is explicitly non-normative and,
//! since the `det-skx-v1` → `det-cfl-v1` re-baseline, describes the old model;
//! the TOML wins and is what `build.rs` parses.
#![cfg_attr(not(test), no_std)]

/// One frozen CPUID (leaf, subleaf) and its expected register values. A
/// register flagged in `dyn_mask` is a pure function of guest state (OSXSAVE
/// mirror, level echo, XSAVE size): the payload reports it but does not
/// exact-compare it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuidEntry {
    /// CPUID leaf (EAX input).
    pub leaf: u32,
    /// CPUID subleaf (ECX input); a single concrete probe per contract row.
    pub subleaf: u32,
    /// Expected EAX (or `dyn` base).
    pub eax: u32,
    /// Expected EBX (or `dyn` base).
    pub ebx: u32,
    /// Expected ECX (or `dyn` base).
    pub ecx: u32,
    /// Expected EDX (or `dyn` base).
    pub edx: u32,
    /// Bit i (0=EAX..3=EDX) set ⇒ register i is dynamic (skip exact compare).
    pub dyn_mask: u8,
}

impl CpuidEntry {
    /// Expected value of register `i` (0=EAX, 1=EBX, 2=ECX, 3=EDX).
    pub const fn reg(&self, i: usize) -> u32 {
        match i {
            0 => self.eax,
            1 => self.ebx,
            2 => self.ecx,
            _ => self.edx,
        }
    }

    /// Whether register `i` is dynamic (guest-state-dependent).
    pub const fn is_dyn(&self, i: usize) -> bool {
        self.dyn_mask & (1 << i) != 0
    }
}

/// An MSR whose read returns a fixed contract value (allow-fixed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsrFixed {
    /// MSR index (ECX for RDMSR).
    pub index: u32,
    /// The value a contract-conforming RDMSR returns.
    pub value: u64,
}

include!(concat!(env!("OUT_DIR"), "/contract_generated.rs"));

/// True iff `index` is named anywhere in the contract (any disposition). The
/// complement is the default-deny surface the `msr-denied` payload probes.
pub fn is_contract_msr(index: u32) -> bool {
    MSR_CONTRACT_RANGES
        .binary_search_by(|&(lo, hi)| {
            if index < lo {
                core::cmp::Ordering::Greater
            } else if index > hi {
                core::cmp::Ordering::Less
            } else {
                core::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// A spread of MSR indices that are **absent** from the contract, so the
/// default-deny rule (`#GP` on the box) applies. Hand-picked across the index
/// space; `denied_samples_are_unknown` proves each is truly off-contract.
pub static MSR_DENIED_SAMPLE: &[u32] = &[
    0x0000_0000, // IA32_P5_MC_ADDR — legacy, not modeled
    0x0000_0013, // gap between MSR_KVM_SYSTEM_TIME (0x12) and PLATFORM_ID (0x17)
    0x0000_0039, // gap below MSR_IA32_FEAT_CTL neighbourhood
    0x0000_0150, // undefined Intel index
    0x0000_0fff, // below the 0x1000 architectural-LBR block
    0x0000_2000, // unassigned
    0x0001_0000, // unassigned
    0x4000_0fff, // Hyper-V leaf gap (above modeled 0x400000ff..0x40000118)
    0xc000_0200, // between AMD perf-global and K7 blocks
    0xdead_beef, // arbitrary high index
    0xffff_ffff, // top of the index space
];

/// How the `msr-allowed` payload round-trips one allow-stateful MSR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoundtripKind {
    /// Write [`MsrRoundtrip::value`] verbatim; the read-back must equal it.
    Exact,
    /// Read-modify-write: write `orig ^ value` (so `value` is a *toggle mask*) and
    /// require the read-back to equal `orig ^ value`. For MSRs that must preserve
    /// live bits — `EFER` under a 64-bit guest, where only `SCE` may toggle while
    /// `LME`/`LMA` must persist (clearing them faults).
    Toggle,
}

/// The contract-legal write the `msr-allowed` payload uses to round-trip one
/// allow-stateful MSR. The value is chosen to be *architecturally legal* for the
/// index (canonical address, valid memory-type encoding, reserved bits clear) so
/// the round-trip exercises contract behavior, not a `#GP` — see
/// [`roundtrip_value`] for the per-index reasoning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsrRoundtrip {
    /// MSR index (ECX for RD/WRMSR).
    pub index: u32,
    /// Exact write value, or a toggle mask when `kind == Toggle`.
    pub value: u64,
    /// How to apply `value` (verbatim vs. read-modify-write toggle).
    pub kind: RoundtripKind,
}

/// A high-canonical 48-bit-VA address (bits 63:47 all set, sign-extending bit 47),
/// made distinct per `index`. Legal for every MSR that requires a canonical write
/// (`FS_BASE`/`GS_BASE`/`KERNEL_GS_BASE`/`LSTAR`/`CSTAR`/`SYSENTER_EIP`/`_ESP`):
/// the index only sets bits 12..=43, so bits 63:47 stay all-ones — canonical for
/// any `u32` index.
///
/// `+` composes the two *disjoint* bit-fields (the high all-ones prefix and the
/// shifted index never overlap), so it is exactly `|` here while remaining
/// arithmetically distinct from `^`/`-`/`*` — i.e. no field aliasing.
pub const fn canonical_addr(index: u32) -> u64 {
    0xffff_8000_0000_0000 + ((index as u64) << 12)
}

/// The contract-legal round-trip plan for one allow-stateful MSR `index`.
///
/// The set of indices swept is the contract's **allow-stateful** set
/// ([`MSR_ALLOWED_STATEFUL`], generated from the TOML); this function only
/// supplies the *value* to write. Legality per architecture (so a mismatch is a
/// contract bug, never a `#GP` we mis-read as a non-round-trip):
///
/// | MSR(s) | value | why it is legal |
/// |--------|-------|-----------------|
/// | `EFER 0xc0000080` | toggle `SCE` (bit 0) | RMW; `LME`/`LMA`/`NXE` preserved under the 64-bit guest |
/// | `SYSENTER_CS 0x174` | `0x08` | 32-bit selector field |
/// | `STAR 0xc0000081` | `0x0023001b00000000` | freely-writable SYSCALL/SYSRET selectors |
/// | `SYSCALL_MASK 0xc0000084` | `0x00047700` | 32-bit RFLAGS mask (high dword zero) |
/// | `TSC_AUX 0xc0000103` | `0xc0ffee03` | 32-bit aux (high dword must be zero) |
/// | `LSTAR/CSTAR/SYSENTER_EIP/_ESP/FS_BASE/GS_BASE/KERNEL_GS_BASE` | [`canonical_addr`] | canonical linear address |
/// | `CR_PAT 0x277` | `0x0004070600040706` | 8 valid PAT type bytes; **not** the reset PAT, so a dropped write can't pass vacuously |
/// | `MTRRdefType 0x2ff` | `0x0c04` | `E`\|`FE`\|type=WT, reserved bits clear (WT, not WB, so != QEMU default `0xc06`) |
/// | fixed-range MTRRs (`0x250/0x258/0x259/0x268..=0x26f`) | `0x0605040100040506` | non-uniform valid memory-type bytes (!= QEMU's all-WB/WP/0 defaults) |
/// | variable MTRR PHYSBASEn (`0x200..=0x20e` even) | `(index<<12)\|WB` | base ≤ MAXPHYADDR, type=WB, reserved clear |
/// | variable MTRR PHYSMASKn (`0x201..=0x20f` odd) | `(index<<12)\|Valid` | mask ≤ MAXPHYADDR, Valid set, reserved clear |
///
/// Each value is also chosen **distinct from the MSR's reset/boot value** (e.g.
/// CR_PAT is not the reset PAT; the rest are non-zero while their reset is zero),
/// so a silently-dropped `WRMSR` leaves `read-back == orig != written` and fails
/// the round-trip — never a vacuous pass. The payload asserts `written != orig`
/// to keep that property honest for every index.
///
/// Any future allow-stateful index not listed falls to the default: write zero
/// (broadly legal — round-trips on any MSR that accepts 0). A new MSR needing a
/// non-zero legal value will fail the box round-trip loudly, which is the signal
/// to add an explicit arm (or escalate a mis-classification to the integrator).
pub fn roundtrip_value(index: u32) -> MsrRoundtrip {
    let (value, kind) = match index {
        // EFER: only SCE may toggle; LME/LMA must persist under the 64-bit guest.
        0xc000_0080 => (1, RoundtripKind::Toggle),
        // SYSENTER_CS: a 32-bit selector field.
        0x0000_0174 => (0x0000_0000_0000_0008, RoundtripKind::Exact),
        // STAR: freely-writable SYSCALL/SYSRET selectors (full 64-bit).
        0xc000_0081 => (0x0023_001b_0000_0000, RoundtripKind::Exact),
        // SYSCALL_MASK (IA32_FMASK): a 32-bit RFLAGS mask; high dword stays zero.
        0xc000_0084 => (0x0000_0000_0004_7700, RoundtripKind::Exact),
        // TSC_AUX: 32-bit RDTSCP/RDPID aux; the high dword must be zero.
        0xc000_0103 => (0x0000_0000_c0ff_ee03, RoundtripKind::Exact),
        // Canonical-address MSRs: a high-canonical (sign-extended) linear address.
        0x0000_0175 | 0x0000_0176 | 0xc000_0082 | 0xc000_0083 | 0xc000_0100 | 0xc000_0101
        | 0xc000_0102 => (canonical_addr(index), RoundtripKind::Exact),
        // CR_PAT: eight valid PAT type bytes — the reset PAT with WT<->UC-
        // swapped, so `written` differs from the live reset value (a silently
        // dropped WRMSR cannot pass the round-trip vacuously).
        0x0000_0277 => (0x0004_0706_0004_0706, RoundtripKind::Exact),
        // MTRRdefType: MTRR-enable | fixed-enable | default type WT(4). Type is
        // WT (not WB) so it differs from QEMU's firmware default `0xc06`.
        0x0000_02ff => (0x0000_0000_0000_0c04, RoundtripKind::Exact),
        // Fixed-range MTRRs: a spread of valid memory-type bytes ({UC,WC,WT,WP,WB}),
        // NOT a uniform fill — QEMU initializes these to all-WB/all-WP/0, so a
        // uniform value would equal the live value and round-trip vacuously.
        0x0000_0250 | 0x0000_0258 | 0x0000_0259 | 0x0000_0268..=0x0000_026f => {
            (0x0605_0401_0004_0506, RoundtripKind::Exact)
        }
        // Variable MTRR PHYSBASEn (even index): base + memory-type WB. `+`
        // composes disjoint fields (base in 12.., type in 7:0) — exactly `|` here.
        0x0000_0200..=0x0000_020f if index & 1 == 0 => {
            (((index as u64) << 12) + 0x06, RoundtripKind::Exact)
        }
        // Variable MTRR PHYSMASKn (odd index): mask + Valid (bit 11), disjoint fields.
        0x0000_0200..=0x0000_020f => (((index as u64) << 12) + 0x800, RoundtripKind::Exact),
        // Future allow-stateful MSR: write zero (broadly legal); see the doc table.
        _ => (0, RoundtripKind::Exact),
    };
    MsrRoundtrip { index, value, kind }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spot-check the generated CPUID model against known frozen `det-cfl-v1`
    /// values (gate 3: expected values trace to the contract, not hand-entered).
    #[test]
    fn cpuid_model_matches_contract() {
        let find = |leaf: u32, subleaf: u32| {
            *CPUID_ENTRIES
                .iter()
                .find(|e| e.leaf == leaf && e.subleaf == subleaf)
                .unwrap_or_else(|| panic!("missing CPUID {leaf:#x}.{subleaf}"))
        };
        // Leaf 0: vendor "GenuineIntel", max basic leaf 0x20.
        let l0 = find(0, 0);
        assert_eq!(
            (l0.eax, l0.ebx, l0.ecx, l0.edx),
            (0x20, 0x756e6547, 0x6c65746e, 0x49656e69)
        );
        // Leaf 1: family/model/stepping 06_9e_0c (Coffee Lake); ECX is dynamic
        // (OSXSAVE mirror), EAX/EBX/EDX are static.
        let l1 = find(1, 0);
        assert_eq!(l1.eax, 0x0009_06ec);
        assert!(l1.is_dyn(2) && !l1.is_dyn(0) && !l1.is_dyn(3));
        // Leaf 0xd.0 EBX is dynamic (XSAVE size).
        assert!(find(0xd, 0).is_dyn(1));
        assert_eq!(MAX_BASIC_LEAF, 0x20);
        assert_eq!(MAX_EXTENDED_LEAF, 0x8000_0008);
        assert_eq!(TSC_HZ, 2_000_000_000);
        assert!(
            CPUID_ENTRIES.len() >= 40,
            "expected the full frozen model, got {}",
            CPUID_ENTRIES.len()
        );
    }

    /// allow-fixed values trace to the contract (gate 3): spot-check a spread.
    #[test]
    fn msr_fixed_values_match_contract() {
        let val = |index: u32| {
            MSR_ALLOWED_FIXED
                .iter()
                .find(|m| m.index == index)
                .unwrap_or_else(|| panic!("missing allow-fixed MSR {index:#x}"))
                .value
        };
        assert_eq!(val(0x1b), 0x0000_0000_fee0_0900); // APIC_BASE
        assert_eq!(val(0xce), 0x0000_0000_0000_1400); // PLATFORM_INFO
        assert_eq!(val(0x1a0), 0x0000_0000_0000_1801); // MISC_ENABLE
        assert_eq!(val(0x10a), 0x0000_0000_0a00_0c09); // ARCH_CAPABILITIES
        assert!(MSR_ALLOWED_FIXED.len() >= 8);
    }

    /// Every denied sample is genuinely off-contract (so default-deny applies).
    #[test]
    fn denied_samples_are_unknown() {
        for &i in MSR_DENIED_SAMPLE {
            assert!(
                !is_contract_msr(i),
                "{i:#x} IS in the contract; pick another sample"
            );
        }
        // And known-present indices are reported present (non-vacuous).
        assert!(is_contract_msr(0x10)); // TSC
        assert!(is_contract_msr(0x174)); // SYSENTER_CS
        assert!(is_contract_msr(0x800)); // x2APIC base
        assert!(is_contract_msr(0xc000_0080)); // EFER
    }

    /// Re-parse `docs/cpu-msr-contract.toml` independently of `build.rs` and
    /// return the contract's **allow-stateful** index set. A second, minimal
    /// parser (not the codegen one) so the completeness gate cannot be fooled by a
    /// `build.rs` regression.
    fn contract_allow_stateful_indices() -> std::collections::BTreeSet<u32> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../docs/cpu-msr-contract.toml"
        );
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let hex = |s: &str| {
            let s = s.trim().trim_matches('"');
            u32::from_str_radix(s.trim_start_matches("0x"), 16)
                .unwrap_or_else(|_| panic!("bad hex index {s:?}"))
        };
        let mut out = std::collections::BTreeSet::new();
        // Each `split("[[")` chunk is one array entry's body (header + keys up to
        // the next entry); keep only `msr.entry` chunks that read allow-stateful.
        for chunk in text.split("[[").skip(1) {
            if !chunk.starts_with("msr.entry]]") {
                continue;
            }
            let mut stateful = false;
            let (mut single, mut lo, mut hi, mut members) = (None, None, None, None);
            for line in chunk.lines().map(str::trim) {
                if let Some(v) = line.strip_prefix("read = ") {
                    stateful = v.trim().trim_matches('"') == "allow-stateful";
                } else if let Some(v) = line.strip_prefix("index = ") {
                    single = Some(hex(v));
                } else if let Some(v) = line.strip_prefix("index-lo = ") {
                    lo = Some(hex(v));
                } else if let Some(v) = line.strip_prefix("index-hi = ") {
                    hi = Some(hex(v));
                } else if let Some(v) = line.strip_prefix("index-members = ") {
                    members = Some(v.to_string());
                }
            }
            if !stateful {
                continue;
            }
            if let Some(i) = single {
                out.insert(i);
            }
            if let (Some(lo), Some(hi)) = (lo, hi) {
                out.extend(lo..=hi);
            }
            if let Some(m) = members {
                for tok in m.trim().trim_matches(['[', ']'].as_ref()).split(',') {
                    let tok = tok.trim();
                    if !tok.is_empty() {
                        out.insert(hex(tok));
                    }
                }
            }
        }
        out
    }

    /// Gate 1 (completeness, mechanized): the swept set — what the payload
    /// iterates ([`MSR_ALLOWED_STATEFUL`]) — equals the contract's allow-stateful
    /// set parsed straight from the TOML. Adding an allow-stateful row to the
    /// contract without the sweep picking it up fails here, loudly.
    #[test]
    fn sweep_set_equals_contract_allow_stateful() {
        let swept: std::collections::BTreeSet<u32> = MSR_ALLOWED_STATEFUL.iter().copied().collect();
        let contract = contract_allow_stateful_indices();
        assert_eq!(
            swept,
            contract,
            "swept allow-stateful set != contract allow-stateful set\n  \
             only-swept:   {:#x?}\n  only-contract: {:#x?}",
            swept.difference(&contract).collect::<Vec<_>>(),
            contract.difference(&swept).collect::<Vec<_>>(),
        );
        // Non-vacuous: the contract really does carry the boot-path MSRs.
        assert!(!swept.is_empty());
        for boot_msr in [0xc000_0080, 0xc000_0082, 0x0000_0277, 0x0000_0200] {
            assert!(
                swept.contains(&boot_msr),
                "{boot_msr:#x} missing from the sweep"
            );
        }
    }

    /// A valid IA32_MTRRcap / variable-MTRR memory-type encoding (UC/WC/WT/WP/WB).
    fn is_valid_memtype(t: u8) -> bool {
        matches!(t, 0 | 1 | 4 | 5 | 6)
    }

    /// A valid PAT entry encoding (memory types plus UC- = 7).
    fn is_valid_pat_type(t: u8) -> bool {
        matches!(t, 0 | 1 | 4 | 5 | 6 | 7)
    }

    /// Every byte lane of `v` is a valid memory type per `ok`.
    fn all_bytes_valid(v: u64, ok: impl Fn(u8) -> bool) -> bool {
        v.to_le_bytes().into_iter().all(ok)
    }

    /// `addr` is canonical for a 48-bit-VA machine (bits 63:47 all equal).
    fn is_canonical(addr: u64) -> bool {
        let top = addr >> 47;
        top == 0 || top == 0x1_ffff
    }

    /// Gate 2 support (legality, mechanized): the write value [`roundtrip_value`]
    /// picks for every swept MSR is architecturally legal for that index, so a box
    /// round-trip failure is a real contract problem, never a `#GP` from an illegal
    /// probe value. `MAXPHYADDR=39` on `det-cfl-v1` (CPUID 0x80000008 EAX low byte).
    #[test]
    fn roundtrip_values_are_legal() {
        const MAXPHYADDR: u32 = 39;
        let phys_reserved = !((1u64 << MAXPHYADDR) - 1); // bits >= MAXPHYADDR
        for &idx in MSR_ALLOWED_STATEFUL {
            let plan = roundtrip_value(idx);
            assert_eq!(plan.index, idx);
            let v = plan.value;
            match idx {
                0xc000_0080 => {
                    // EFER toggles SCE only (bit 0) via read-modify-write.
                    assert_eq!(plan.kind, RoundtripKind::Toggle);
                    assert_eq!(v, 1, "EFER must toggle SCE (bit 0) only");
                }
                0x0000_0175 | 0x0000_0176 | 0xc000_0082 | 0xc000_0083 | 0xc000_0100
                | 0xc000_0101 | 0xc000_0102 => {
                    assert_eq!(plan.kind, RoundtripKind::Exact);
                    assert!(is_canonical(v), "{idx:#x}: {v:#x} not canonical");
                }
                0x0000_0277 => assert!(
                    all_bytes_valid(v, is_valid_pat_type),
                    "CR_PAT {v:#x} has an invalid PAT byte"
                ),
                0x0000_02ff => {
                    // Only type (bits 7:0), FE (bit 10) and E (bit 11) may be set;
                    // everything else (9:8, 63:12) is reserved and must be clear.
                    assert!(is_valid_memtype((v & 0xff) as u8), "MTRRdefType bad type");
                    assert_eq!(v & !0x0000_0cffu64, 0, "MTRRdefType reserved bit set");
                }
                0x0000_0250 | 0x0000_0258 | 0x0000_0259 | 0x0000_0268..=0x0000_026f => assert!(
                    all_bytes_valid(v, is_valid_memtype),
                    "fixed MTRR {idx:#x}={v:#x} has an invalid memory type"
                ),
                0x0000_0200..=0x0000_020f if idx & 1 == 0 => {
                    // PHYSBASEn: type valid, bits 11:8 reserved, base <= MAXPHYADDR.
                    assert!(is_valid_memtype((v & 0xff) as u8), "{idx:#x} bad memtype");
                    assert_eq!(v & 0x0f00, 0, "{idx:#x} PHYSBASE reserved 11:8 set");
                    assert_eq!(v & phys_reserved, 0, "{idx:#x} PHYSBASE above MAXPHYADDR");
                }
                0x0000_0200..=0x0000_020f => {
                    // PHYSMASKn: Valid (bit 11) set, bits 10:0 reserved, <= MAXPHYADDR.
                    assert_eq!(v & 0x800, 0x800, "{idx:#x} PHYSMASK Valid bit clear");
                    assert_eq!(v & 0x7ff, 0, "{idx:#x} PHYSMASK reserved 10:0 set");
                    assert_eq!(v & phys_reserved, 0, "{idx:#x} PHYSMASK above MAXPHYADDR");
                }
                // Selector / freely-writable / 32-bit-width MSRs: no reserved-bit
                // hazard for the chosen values; nothing extra to assert.
                _ => {}
            }
        }
    }

    /// Exact-value pin (mutation oracle): an **independent literal table** of the
    /// `(value, kind)` `roundtrip_value` must return for every swept MSR. The
    /// `roundtrip_values_are_legal` test only checks *properties* (canonical /
    /// valid memtype / reserved-clear), so a value mutated to another *legal*
    /// value would slip past it; this pins the exact bytes, so any drift in a
    /// constant, a `<<`/`+` operator, a match arm, or the even/odd guard fails
    /// here. A newly-swept index with no pin is a loud `panic` (forces a pin).
    #[test]
    fn roundtrip_values_are_exactly_pinned() {
        use RoundtripKind::{Exact, Toggle};
        let expect = |idx: u32| -> (u64, RoundtripKind) {
            match idx {
                0x0000_0174 => (0x0000_0000_0000_0008, Exact), // SYSENTER_CS
                0x0000_0175 => (0xffff_8000_0017_5000, Exact), // SYSENTER_ESP (canonical)
                0x0000_0176 => (0xffff_8000_0017_6000, Exact), // SYSENTER_EIP (canonical)
                0x0000_0200 => (0x0000_0000_0020_0006, Exact), // MTRR PHYSBASE0 (base|WB)
                0x0000_0201 => (0x0000_0000_0020_1800, Exact), // MTRR PHYSMASK0 (mask|Valid)
                0x0000_0202 => (0x0000_0000_0020_2006, Exact),
                0x0000_0203 => (0x0000_0000_0020_3800, Exact),
                0x0000_0204 => (0x0000_0000_0020_4006, Exact),
                0x0000_0205 => (0x0000_0000_0020_5800, Exact),
                0x0000_0206 => (0x0000_0000_0020_6006, Exact),
                0x0000_0207 => (0x0000_0000_0020_7800, Exact),
                0x0000_0208 => (0x0000_0000_0020_8006, Exact),
                0x0000_0209 => (0x0000_0000_0020_9800, Exact),
                0x0000_020a => (0x0000_0000_0020_a006, Exact),
                0x0000_020b => (0x0000_0000_0020_b800, Exact),
                0x0000_020c => (0x0000_0000_0020_c006, Exact),
                0x0000_020d => (0x0000_0000_0020_d800, Exact),
                0x0000_020e => (0x0000_0000_0020_e006, Exact),
                0x0000_020f => (0x0000_0000_0020_f800, Exact),
                // Fixed-range MTRRs: a non-uniform spread of valid memory types.
                0x0000_0250 | 0x0000_0258 | 0x0000_0259 | 0x0000_0268..=0x0000_026f => {
                    (0x0605_0401_0004_0506, Exact)
                }
                0x0000_02ff => (0x0000_0000_0000_0c04, Exact), // MTRRdefType (E|FE|WT)
                0x0000_0277 => (0x0004_0706_0004_0706, Exact), // CR_PAT (non-reset legal PAT)
                0xc000_0080 => (0x0000_0000_0000_0001, Toggle), // EFER: toggle SCE
                0xc000_0081 => (0x0023_001b_0000_0000, Exact), // STAR
                0xc000_0082 => (0xffff_8c00_0008_2000, Exact), // LSTAR (canonical)
                0xc000_0083 => (0xffff_8c00_0008_3000, Exact), // CSTAR (canonical)
                0xc000_0084 => (0x0000_0000_0004_7700, Exact), // SYSCALL_MASK
                0xc000_0100 => (0xffff_8c00_0010_0000, Exact), // FS_BASE (canonical)
                0xc000_0101 => (0xffff_8c00_0010_1000, Exact), // GS_BASE (canonical)
                0xc000_0102 => (0xffff_8c00_0010_2000, Exact), // KERNEL_GS_BASE (canonical)
                0xc000_0103 => (0x0000_0000_c0ff_ee03, Exact), // TSC_AUX
                other => panic!("no pinned round-trip value for swept MSR {other:#x} — add one"),
            }
        };
        let mut n = 0;
        for &idx in MSR_ALLOWED_STATEFUL {
            let plan = roundtrip_value(idx);
            assert_eq!(plan.index, idx);
            assert_eq!(
                (plan.value, plan.kind),
                expect(idx),
                "{idx:#x}: roundtrip_value drifted from its pinned (value, kind)"
            );
            n += 1;
        }
        assert_eq!(n, 41, "expected 41 allow-stateful MSRs swept, got {n}");
    }

    /// The merged ranges are sorted and disjoint (binary search precondition).
    #[test]
    fn contract_ranges_are_sorted_disjoint() {
        for w in MSR_CONTRACT_RANGES.windows(2) {
            assert!(
                w[0].1 < w[1].0,
                "ranges overlap/adjacent: {:?} {:?}",
                w[0],
                w[1]
            );
            assert!(w[0].0 <= w[0].1);
        }
    }
}
