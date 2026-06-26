// SPDX-License-Identifier: AGPL-3.0-or-later
//! CPU-MSR-CONTRACT §1.1/§1.2 host-homogeneity enforcement: the assertions
//! `vmm-core` checks against the **physical host** at VM start and **refuses to
//! run** on any mismatch.
//!
//! The determinism guarantee is defined over a homogeneous, single-tenant,
//! pinned-core fleet (§1.1): two runs of the same guest+seed are bit-identical
//! **only** when every host is the frozen baseline. A host outside that domain
//! would diverge in native (non-trapping) instruction/FPU behavior — XSAVE image
//! layout, `#UD`-vs-execute decisions, MAXPHYADDR-dependent paging — while still
//! claiming the frozen CPUID/MSR contract. So [`enforce`] (called first by
//! [`crate::bringup::boot`], before any policy install or guest entry) probes the
//! live CPU and fails closed.
//!
//! The probe reads the **physical host** (host `CPUID`, the FXSAVE `MXCSR_MASK`,
//! the kernel-recorded microcode revision) and is therefore intrinsically
//! **box-only**: it is meaningful only on the Linux/x86-64 bare-metal box the
//! guest actually executes on. On any other target — a dev Mac, a non-x86 host,
//! or under Miri (which cannot run `mmap`, `CPUID`, or `FXSAVE`) — there is no
//! physical guest execution to protect, so the probe is skipped and [`enforce`]
//! is a no-op. This `cfg(target_os/target_arch)` seam is the same box-only live
//! boundary the task already draws for `KVM_RUN`; it is documented in
//! `IMPLEMENTATION.md` as a declared exception to conventions rule 6.

use crate::vmm::VmmError;

/// One host-baseline assertion's evaluation: the contract key, the expected
/// value, what the live host actually presented, and whether it satisfies the
/// assertion. Surfaced by [`report`] so the integrator can see every assertion's
/// disposition — not just the first failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outcome {
    /// The §1.1/§6 `host-assert` key (e.g. `family-model-stepping`,
    /// `host-absent RDPID`).
    pub key: String,
    /// The contract's expected value / predicate.
    pub expected: String,
    /// What the live host presented.
    pub actual: String,
    /// Whether the host satisfies this assertion.
    pub pass: bool,
}

impl Outcome {
    fn new(
        key: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
        pass: bool,
    ) -> Self {
        Self {
            key: key.into(),
            expected: expected.into(),
            actual: actual.into(),
            pass,
        }
    }
}

/// Evaluate every §1.1 host-baseline assertion against the live CPU and return
/// each one's [`Outcome`]. On a non-box host (not Linux/x86-64, or under Miri) the
/// physical determinism domain does not apply — there is no real guest execution
/// — so a single skipped, passing outcome is returned.
pub fn report() -> Vec<Outcome> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
    {
        probe::evaluate()
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64", not(miri))))]
    {
        vec![Outcome::new(
            "host-baseline",
            "Linux x86-64 bare-metal box in the det-cfl-v1 determinism domain",
            "not a Linux/x86-64 host (or under Miri) — host-assert is box-only; skipped",
            true,
        )]
    }
}

/// Enforce the §1.1 host-homogeneity baseline: return [`VmmError::HostAssert`]
/// (listing every failed assertion) if the live host is outside the frozen
/// determinism domain, else `Ok`. Called by [`crate::bringup::boot`] **before**
/// installing the CPUID/MSR policy or entering the guest. A no-op off the box
/// (where there is no physical guest to protect).
pub(crate) fn enforce() -> Result<(), VmmError> {
    verdict(report())
}

/// `Ok` iff every outcome passed, else `HostAssert` listing the failures. Split out
/// of [`enforce`] so the all-pass-vs-any-fail decision is unit-testable with
/// synthetic outcomes on every platform (the live `report()` on a non-baseline box
/// always carries failures, which cannot exercise the `Ok` branch).
fn verdict(outcomes: Vec<Outcome>) -> Result<(), VmmError> {
    let failures: Vec<String> = outcomes
        .into_iter()
        .filter(|o| !o.pass)
        .map(|o| format!("{} (expected {}, observed {})", o.key, o.expected, o.actual))
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(VmmError::HostAssert(failures.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `report()` runs on **every** platform and `enforce()` consumes it. On a
    /// non-box host (Mac, Miri, non-x86) it yields the single skipped/passing
    /// outcome; on the Linux/x86-64 box it drives the live `probe::evaluate()`
    /// (covering the CPUID / FXSAVE / sysfs reads). Either way every outcome is
    /// well-formed, and `enforce()`'s verdict matches "all assertions passed". This
    /// keeps the probe exercised in the default coverage lane now that the live
    /// M1/M2 tests are `#[ignore]`d.
    #[test]
    fn report_is_well_formed_and_enforce_agrees() {
        let report = report();
        assert!(!report.is_empty(), "report is never empty");
        for o in &report {
            assert!(!o.key.is_empty(), "every outcome names its key");
            assert!(
                !o.expected.is_empty(),
                "every outcome states the expectation"
            );
            assert!(
                !o.actual.is_empty(),
                "every outcome states what was observed"
            );
        }
        // enforce() returns Ok iff every assertion passed.
        let all_pass = report.iter().all(|o| o.pass);
        assert_eq!(enforce().is_ok(), all_pass);
    }

    #[test]
    fn verdict_is_ok_iff_all_outcomes_pass() {
        let pass = Outcome::new("k", "e", "a", true);
        let fail = Outcome::new("k2", "e2", "a2", false);
        // All-pass (and vacuous) ⇒ Ok; any failure ⇒ Err (kills the `!o.pass`
        // filter mutant, which the live mixed-result box report cannot exercise).
        assert!(verdict(vec![]).is_ok());
        assert!(verdict(vec![pass.clone()]).is_ok());
        assert!(verdict(vec![pass.clone(), pass.clone()]).is_ok());
        assert!(verdict(vec![fail.clone()]).is_err());
        assert!(verdict(vec![pass, fail]).is_err());
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", not(miri)))]
mod probe {
    //! The live-CPU probe (box-only). `CPUID` is a safe intrinsic on x86-64; the
    //! one `unsafe` is `_fxsave64` (a box-only read of host FPU save state into a
    //! local aligned buffer — no guest effect), excluded from the Miri unsafe
    //! surface by the module `cfg`, exactly like the `GuestRam` mmap path.

    use core::arch::x86_64::{__cpuid, __cpuid_count, _fxsave64, CpuidResult};

    use super::Outcome;
    use crate::contract::host_expectations;

    /// Evaluate every §1.1 assertion against the live CPU.
    pub(super) fn evaluate() -> Vec<Outcome> {
        let exp = host_expectations();
        let mut out = Vec::new();

        // (1) family/model/stepping — host CPUID(1).EAX.
        let fms = fmt_fms(cpuid(1).eax);
        out.push(Outcome::new(
            "family-model-stepping",
            exp.family_model_stepping,
            &fms,
            fms == exp.family_model_stepping,
        ));

        // (2) host microcode revision — the kernel-recorded value (RDMSR 0x8b is
        //     ring-0-only, so a userspace VMM reads sysfs/cpuinfo, not the MSR).
        let (uc_actual, uc_pass) = match read_microcode_rev() {
            Some(rev) => (fmt_hex(rev), rev == exp.microcode_rev),
            None => ("unreadable".to_string(), false),
        };
        out.push(Outcome::new(
            "host-microcode-rev",
            fmt_hex(exp.microcode_rev),
            uc_actual,
            uc_pass,
        ));

        // (3) MXCSR_MASK — FXSAVE save-area offset 28 (a host CPU constant).
        let mask = fxsave_mxcsr_mask();
        out.push(Outcome::new(
            "mxcsr-mask",
            format!("{:#06x}", exp.mxcsr_mask),
            format!("{mask:#06x}"),
            mask == exp.mxcsr_mask,
        ));

        // (4) MAXPHYADDR — host CPUID(0x8000_0008).EAX[7:0], must be >= the min.
        let maxpa = cpuid(0x8000_0008).eax & 0xff;
        out.push(Outcome::new(
            "maxphyaddr-min",
            format!(">= {}", exp.maxphyaddr_min),
            maxpa.to_string(),
            maxpa >= exp.maxphyaddr_min,
        ));

        // (5) rtm-disabled — RTM must be non-usable by the guest. The contract's
        //     `rtm-disabled` is satisfied two ways: RTM physically absent
        //     (CPUID.7.0:EBX[11]=0 → XBEGIN `#UD`s), OR vmm-core actually installs
        //     the `IA32_TSX_CTRL = RTM_DISABLE | TSX_CPUID_CLEAR` pin before
        //     `KVM_RUN`. **This skeleton does NOT install that pin** (it is a
        //     backend/VMCS concern, a later phase), so the *only* honest pass here
        //     is physical absence — the mere existence of the `IA32_TSX_CTRL` MSR
        //     does **not** make RTM non-usable, and claiming a pass on that basis
        //     would let a TSX-capable host run native (nondeterministic) RTM. So a
        //     host with RTM present fails until the pin install is wired.
        if exp.rtm_disabled {
            let rtm = bit(cpuid_count(7, 0).ebx, 11);
            let actual = if rtm {
                "rtm-present, and this skeleton does not install the IA32_TSX_CTRL pin \
                 (RTM_DISABLE|TSX_CPUID_CLEAR) before KVM_RUN — RTM would run native"
                    .to_string()
            } else {
                "rtm-absent (XBEGIN #UDs; already non-usable)".to_string()
            };
            out.push(Outcome::new(
                "rtm-disabled",
                "rtm physically absent (the IA32_TSX_CTRL pin is not installed by this skeleton)",
                actual,
                !rtm,
            ));
        }

        // (6) host-absent — every variance instruction the contract relies on
        //     faulting must be physically absent (fail closed on an unknown name).
        for mnem in exp.host_absent {
            let (present, known) = insn_present(mnem);
            let actual = match (known, present) {
                (false, _) => "unrecognized mnemonic — cannot verify absence".to_string(),
                (true, true) => "present".to_string(),
                (true, false) => "absent".to_string(),
            };
            out.push(Outcome::new(
                format!("host-absent {mnem}"),
                "absent",
                actual,
                known && !present,
            ));
        }

        out
    }

    // CPUID is unconditionally available on x86-64, so `__cpuid`/`__cpuid_count`
    // are **safe** intrinsics (no `unsafe` needed); they read CPU identification
    // registers only, with no memory or guest-visible effect. The only `unsafe`
    // in this module is `_fxsave64` (below).
    fn cpuid(leaf: u32) -> CpuidResult {
        __cpuid(leaf)
    }

    fn cpuid_count(leaf: u32, sub: u32) -> CpuidResult {
        __cpuid_count(leaf, sub)
    }

    fn bit(reg: u32, n: u32) -> bool {
        (reg >> n) & 1 == 1
    }

    /// Decode CPUID(1).EAX into the contract's `ff_mm_ss` hex form (e.g.
    /// `06_55_04` for Skylake-SP stepping 4).
    fn fmt_fms(eax: u32) -> String {
        let base_family = (eax >> 8) & 0xf;
        let base_model = (eax >> 4) & 0xf;
        let stepping = eax & 0xf;
        let ext_model = (eax >> 16) & 0xf;
        let ext_family = (eax >> 20) & 0xff;
        let family = base_family + if base_family == 0xf { ext_family } else { 0 };
        let model = if base_family == 0x6 || base_family == 0xf {
            (ext_model << 4) | base_model
        } else {
            base_model
        };
        format!("{family:02x}_{model:02x}_{stepping:02x}")
    }

    /// Render a 64-bit value in the contract's 16-hex-digit pinned form.
    fn fmt_hex(v: u64) -> String {
        format!("{v:#018x}")
    }

    /// Parse a microcode-revision hex/decimal token (`"0xf8"`, `" 0x0200005e "`)
    /// into a `u64`; `None` if it is not a valid number. Pure — split out so the
    /// number parsing is unit-testable without the host filesystem.
    fn parse_microcode_hex(s: &str) -> Option<u64> {
        let t = s.trim();
        u64::from_str_radix(t.strip_prefix("0x").unwrap_or(t), 16).ok()
    }

    /// Read the physical microcode revision the kernel recorded (RDMSR 0x8b is
    /// ring-0-only). Prefers the sysfs `microcode/version`, falls back to
    /// `/proc/cpuinfo`'s `microcode` line; `None` if neither is readable.
    fn read_microcode_rev() -> Option<u64> {
        if let Ok(s) = std::fs::read_to_string("/sys/devices/system/cpu/cpu0/microcode/version")
            && let Some(v) = parse_microcode_hex(&s)
        {
            return Some(v);
        }
        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
        cpuinfo
            .lines()
            .find_map(|l| l.strip_prefix("microcode").and_then(|r| r.split_once(':')))
            .and_then(|(_, v)| parse_microcode_hex(v))
    }

    /// `MXCSR_MASK` from the FXSAVE save area (offset 28), a host CPU constant.
    fn fxsave_mxcsr_mask() -> u32 {
        #[repr(align(16))]
        struct FxArea([u8; 512]);
        let mut area = FxArea([0u8; 512]);
        // SAFETY: `_fxsave64` stores the 512-byte FXSAVE image to a 16-byte-aligned
        // buffer; `FxArea` is `#[repr(align(16))]` and exactly 512 bytes, so the
        // store is in-bounds and correctly aligned. No guest state is touched.
        // Box-only, outside the Miri unsafe surface.
        unsafe { _fxsave64(area.0.as_mut_ptr().cast()) }
        u32::from_le_bytes([area.0[28], area.0[29], area.0[30], area.0[31]])
    }

    /// `(present, recognized)` for a variance instruction, by its host CPUID
    /// feature bit. An unrecognized mnemonic returns `recognized = false` so the
    /// caller fails closed (an unverifiable absence is never silently a pass).
    fn insn_present(mnemonic: &str) -> (bool, bool) {
        let l7_0 = cpuid_count(7, 0);
        match mnemonic {
            "RDPID" => (bit(l7_0.ecx, 22), true),
            "SERIALIZE" => (bit(l7_0.edx, 14), true),
            "SHA" => (bit(l7_0.ebx, 29), true),
            "PCONFIG" => (bit(l7_0.edx, 18), true),
            "HRESET" => (bit(cpuid_count(7, 1).eax, 22), true),
            // WAITPKG (CPUID.7.0:ECX[5]) gates UMWAIT/TPAUSE/UMONITOR together.
            "UMWAIT" | "TPAUSE" | "UMONITOR" => (bit(l7_0.ecx, 5), true),
            _ => (false, false),
        }
    }

    #[cfg(test)]
    mod tests {
        //! Pure-decoder unit tests (Linux/x86-64 only, where this module compiles)
        //! — they exercise the branches `report()`'s live probe does not reach on
        //! the box (a family-0xF identity, an unrecognized mnemonic, a non-numeric
        //! microcode token), so the box coverage lane covers the decoders fully.

        use super::*;

        #[test]
        fn fmt_fms_decodes_family_model_stepping() {
            // Skylake-SP stepping 4: family 6, ext-model 5 ⇒ model 0x55, stepping 4.
            assert_eq!(fmt_fms(0x0005_0654), "06_55_04");
            // Coffee Lake i9-9900K: family 6, ext-model 9 + base-model 0xE ⇒ 0x9E.
            assert_eq!(fmt_fms(0x0009_06ec), "06_9e_0c");
            // Extended-family path (base family 0xF adds the extended family byte).
            assert_eq!(fmt_fms(0x00f0_0f00), "1e_00_00");
        }

        #[test]
        fn parse_microcode_hex_total() {
            assert_eq!(parse_microcode_hex("0xf8"), Some(0xf8));
            assert_eq!(parse_microcode_hex("  0x0200005e  "), Some(0x0200_005e));
            assert_eq!(parse_microcode_hex("ff"), Some(0xff)); // bare hex
            assert_eq!(parse_microcode_hex("not-a-number"), None);
            assert_eq!(parse_microcode_hex(""), None);
        }

        #[test]
        fn bit_extracts_the_right_position() {
            assert!(bit(0b1000, 3));
            assert!(!bit(0b1000, 2));
            assert!(bit(1 << 31, 31));
        }

        #[test]
        fn insn_present_known_and_unknown() {
            // Every contract mnemonic is recognized (the box happens to lack them).
            for m in [
                "RDPID",
                "SERIALIZE",
                "SHA",
                "PCONFIG",
                "HRESET",
                "UMWAIT",
                "TPAUSE",
                "UMONITOR",
            ] {
                let (_present, known) = insn_present(m);
                assert!(known, "{m} is a recognized variance instruction");
            }
            // An unrecognized mnemonic fails closed (recognized = false).
            assert_eq!(insn_present("BOGUS"), (false, false));
        }
    }
}
