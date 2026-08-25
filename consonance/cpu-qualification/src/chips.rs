// SPDX-License-Identifier: AGPL-3.0-or-later
//! The known-chip table: one entry per supported chip family.
//!
//! The table says what a known chip should look like. It selects which
//! measurements run and nothing more — every entry is then measured on the chip,
//! never trusted. A chip that matches no entry is refused with a machine-readable
//! record of what was found; the suite never guesses an event for unknown silicon.
//!
//! Event configs are traceable to rr's `src/PerfCounters.cc`, and the AMD lock
//! probe to `src/PerfCounters_x86.h`. Where a value here
//! departs from rr's, the entry records what differs.

use serde::{Deserialize, Serialize};

/// A value the table would carry if a source recorded it.
///
/// The table is code, not measurement, so a row whose value has no recorded
/// source states that instead of carrying a plausible number. A rule built on an
/// unrecorded value never matches, so the absence surfaces as a refusal naming the
/// entry rather than as a silently-wrong match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableValue<T: 'static> {
    /// A value and the source it is traceable to.
    Recorded {
        /// The value.
        value: T,
        /// Where it came from.
        source: &'static str,
    },
    /// No source records this value.
    Absent {
        /// Why the table carries no value.
        reason: &'static str,
    },
}

impl<T: Copy> TableValue<T> {
    /// The value, if one is recorded.
    #[must_use]
    pub fn value(self) -> Option<T> {
        match self {
            TableValue::Recorded { value, .. } => Some(value),
            TableValue::Absent { .. } => None,
        }
    }

    /// Why the table carries no value, if it carries none.
    #[must_use]
    pub fn absent_reason(self) -> Option<&'static str> {
        match self {
            TableValue::Absent { reason } => Some(reason),
            TableValue::Recorded { .. } => None,
        }
    }
}

/// The vendor axis. Intel and AMD are the same architecture; the vendor selects
/// the entry, not the instruction set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Vendor {
    /// `GenuineIntel`.
    GenuineIntel,
    /// `AuthenticAMD`.
    AuthenticAMD,
    /// An aarch64 implementer, identified through `MIDR_EL1`.
    Aarch64,
}

impl Vendor {
    /// The CPUID leaf-0 vendor string, for the x86 vendors.
    #[must_use]
    pub fn cpuid_string(self) -> Option<&'static str> {
        match self {
            Vendor::GenuineIntel => Some("GenuineIntel"),
            Vendor::AuthenticAMD => Some("AuthenticAMD"),
            Vendor::Aarch64 => None,
        }
    }
}

/// What a chip must look like for an entry to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchRule {
    /// x86: this family, and one of these models. Stepping is not part of the
    /// rule — a stepping change is a re-certification, not a different entry.
    X86FamilyModels {
        /// The CPUID family, after the extended-family fold.
        family: u32,
        /// The CPUID models, after the extended-model fold.
        models: &'static [u32],
    },
    /// x86: an inclusive family range, models unconstrained.
    X86FamilyRange {
        /// Lowest family the entry covers.
        lo: u32,
        /// Highest family the entry covers.
        hi: u32,
    },
    /// aarch64: `MIDR_EL1` implementer and part number.
    Midr {
        /// `MIDR_EL1` bits 31:24.
        implementer: TableValue<u32>,
        /// `MIDR_EL1` bits 15:4.
        part: TableValue<u32>,
    },
}

/// What the chip's performance-monitoring hardware should look like.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PmuShape {
    /// Intel architectural performance monitoring, at this version.
    IntelArchPerfmon {
        /// The architectural performance-monitoring version.
        version: u32,
    },
    /// AMD core performance monitoring: legacy per-counter MSRs, with PerfMonV2
    /// where the chip advertises it.
    AmdCore,
    /// aarch64 PMUv3, with the work-clock event required to appear in an event
    /// identification register.
    ArmPmuV3 {
        /// The register the work-clock event must be advertised in.
        event_id_register: &'static str,
    },
}

/// A standing host condition an entry requires. Stage 0 checks every kind the
/// entry names; the pack records the state each must be in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostConditionKind {
    /// The NMI watchdog is off.
    NmiWatchdogOff,
    /// The frequency governor is pinned.
    GovernorPinned,
    /// The simultaneous-multithreading policy is the one the baseline requires.
    SmtPolicy,
    /// `/dev/kvm` exists and is usable.
    KvmPresent,
    /// The loaded KVM module is the expected one, by content.
    KvmModuleIdentity,
    /// The measurement thread is pinned to one core.
    CorePinning,
    /// Speculative lock mapping is disabled on every core.
    SpecLockMapDisabled,
    /// The speculative-store-bypass mitigation mode is pinned so the kernel
    /// cannot rewrite the same register.
    SsbMitigationPinned,
    /// The advanced virtual interrupt controller is off.
    AvicOff,
}

impl HostConditionKind {
    /// The token the pack and the report spell this condition with.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            HostConditionKind::NmiWatchdogOff => "nmi-watchdog-off",
            HostConditionKind::GovernorPinned => "governor-pinned",
            HostConditionKind::SmtPolicy => "smt-policy",
            HostConditionKind::KvmPresent => "kvm-present",
            HostConditionKind::KvmModuleIdentity => "kvm-module-identity",
            HostConditionKind::CorePinning => "core-pinning",
            HostConditionKind::SpecLockMapDisabled => "spec-lock-map-disabled",
            HostConditionKind::SsbMitigationPinned => "ssb-mitigation-pinned",
            HostConditionKind::AvicOff => "avic-off",
        }
    }
}

/// One entry of the known-chip table.
#[derive(Clone, Copy, Debug)]
pub struct ChipEntry {
    /// The entry's name, used in reports.
    pub name: &'static str,
    /// The vendor axis.
    pub vendor: Vendor,
    /// What a chip must look like for this entry to apply.
    pub match_rule: MatchRule,
    /// The `PERF_TYPE_RAW` config of the work clock.
    pub work_clock_config: TableValue<u64>,
    /// The vendor's name for the work-clock event.
    pub work_clock_event: &'static str,
    /// rr's config for the same event, when it differs from
    /// [`Self::work_clock_config`].
    pub rr_reference: Option<&'static str>,
    /// What differs from rr's value, when one does.
    pub departure: Option<&'static str>,
    /// The expected performance-monitoring shape.
    pub pmu_shape: PmuShape,
    /// The standing host conditions stage 0 checks for this chip.
    pub host_conditions: &'static [HostConditionKind],
    /// The determinism contract column for this chip.
    pub contract: Option<&'static str>,
    /// The event stage 0 uses to prove the speculative lock-mapping workaround
    /// is actually in effect: retired lock instructions of the speculative
    /// lock-map-commit kind. With the workaround in force a `lock add` produces
    /// none of them, so the probe's pass condition is a count of zero. Only AMD
    /// needs it.
    pub lock_probe_event: Option<TableValue<u64>>,
}

/// Intel Coffee Lake-S: the `det-cfl-v1` baseline.
const INTEL_06_9E: ChipEntry = ChipEntry {
    name: "GenuineIntel 06_9EH",
    vendor: Vendor::GenuineIntel,
    match_rule: MatchRule::X86FamilyModels {
        family: 0x6,
        models: &[0x9e],
        // docs/CPU-QUALIFICATION.md known-chip table; docs/cpu-msr-contract.toml
        // [host-assert] family-model-stepping.
    },
    work_clock_config: TableValue::Recorded {
        value: 0x1c4,
        source: "consonance/vmm-backend/src/arch/x86/mod.rs RAW_BR_COND",
    },
    work_clock_event: "BR_INST_RETIRED.CONDITIONAL",
    rr_reference: Some("0x5101c4"),
    departure: Some(
        "rr's config carries the performance-event-select control bits (0x51 in bits 16-23) \
         in the raw config; this one carries only the event select and unit mask, and sets \
         the counting scope through perf_event_attr fields instead",
    ),
    pmu_shape: PmuShape::IntelArchPerfmon { version: 4 },
    host_conditions: &[
        HostConditionKind::NmiWatchdogOff,
        HostConditionKind::GovernorPinned,
        HostConditionKind::SmtPolicy,
        HostConditionKind::KvmPresent,
        HostConditionKind::KvmModuleIdentity,
        HostConditionKind::CorePinning,
    ],
    contract: Some("docs/cpu-msr-contract.toml"),
    lock_probe_event: None,
};

/// AMD Zen, families 17h through 1Ah.
const AMD_ZEN: ChipEntry = ChipEntry {
    name: "AuthenticAMD families 17h-1Ah",
    vendor: Vendor::AuthenticAMD,
    match_rule: MatchRule::X86FamilyRange { lo: 0x17, hi: 0x1a },
    work_clock_config: TableValue::Recorded {
        value: 0x0051_00d1,
        source: "docs/CPU-QUALIFICATION.md known-chip table, traceable to rr src/PerfCounters.cc",
    },
    work_clock_event: "retired conditional branch instructions",
    rr_reference: Some("0x5100d1"),
    departure: None,
    pmu_shape: PmuShape::AmdCore,
    host_conditions: &[
        HostConditionKind::NmiWatchdogOff,
        HostConditionKind::GovernorPinned,
        HostConditionKind::SmtPolicy,
        HostConditionKind::KvmPresent,
        HostConditionKind::KvmModuleIdentity,
        HostConditionKind::CorePinning,
        HostConditionKind::SpecLockMapDisabled,
        HostConditionKind::SsbMitigationPinned,
        HostConditionKind::AvicOff,
    ],
    contract: Some("docs/cpu-msr-contract-amd-draft.toml"),
    // Event 0x25 is retired lock instructions; unit mask 0x08 selects the
    // speculative lock-map-commit kind, and 0x51 is the enable and user/kernel
    // control field rr carries in the raw config.
    lock_probe_event: Some(TableValue::Recorded {
        value: 0x0051_0825,
        source: "rr src/PerfCounters_x86.h check_for_zen_speclockmap",
    }),
};

/// Arm Neoverse N1.
const NEOVERSE_N1: ChipEntry = ChipEntry {
    name: "Neoverse N1",
    vendor: Vendor::Aarch64,
    match_rule: MatchRule::Midr {
        implementer: TableValue::Absent {
            reason: "the MIDR_EL1 implementer value for Arm Limited is not recorded in this \
                     repository",
        },
        part: TableValue::Absent {
            reason: "the MIDR_EL1 part number for Neoverse N1 is not recorded in this \
                     repository",
        },
    },
    work_clock_config: TableValue::Recorded {
        value: 0x21,
        source: "docs/CPU-QUALIFICATION.md known-chip table",
    },
    work_clock_event: "BR_RETIRED",
    rr_reference: Some("0x21"),
    departure: None,
    pmu_shape: PmuShape::ArmPmuV3 {
        event_id_register: "PMCEID1_EL0",
    },
    host_conditions: &[
        HostConditionKind::KvmPresent,
        HostConditionKind::KvmModuleIdentity,
        HostConditionKind::CorePinning,
        HostConditionKind::GovernorPinned,
    ],
    contract: None,
    lock_probe_event: None,
};

/// The table.
pub const KNOWN_CHIPS: &[ChipEntry] = &[INTEL_06_9E, AMD_ZEN, NEOVERSE_N1];

/// What stage 0 read off the chip, in the shape the table matches against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChipIdentity {
    /// The vendor axis.
    pub vendor: Vendor,
    /// CPUID family after the extended-family fold; unused on aarch64.
    pub family: u32,
    /// CPUID model after the extended-model fold; unused on aarch64.
    pub model: u32,
    /// CPUID stepping; unused on aarch64.
    pub stepping: u32,
    /// `MIDR_EL1`; zero on x86.
    pub midr: u64,
    /// The microcode or firmware revision the kernel records.
    pub microcode_rev: Option<String>,
}

impl ChipIdentity {
    /// The `family_model_stepping` spelling the contract and the pack use.
    #[must_use]
    pub fn family_model_stepping(&self) -> String {
        format!(
            "{:02x}_{:02x}_{:02x}",
            self.family, self.model, self.stepping
        )
    }

    /// `MIDR_EL1` bits 31:24.
    #[must_use]
    pub fn midr_implementer(&self) -> u32 {
        u32::try_from((self.midr >> 24) & 0xff).unwrap_or(0)
    }

    /// `MIDR_EL1` bits 15:4.
    #[must_use]
    pub fn midr_part(&self) -> u32 {
        u32::try_from((self.midr >> 4) & 0xfff).unwrap_or(0)
    }
}

/// Why no table entry applies to a chip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    /// What was found on the chip.
    pub found: ChipIdentity,
    /// Entries that share the chip's vendor but could not be matched, and why.
    /// An entry whose match rule rests on a value no source records lands here,
    /// so an unmatchable entry reads as a refusal rather than as absence.
    pub unmatchable: Vec<String>,
}

/// The table entry for a chip, or a refusal naming what was found.
///
/// # Errors
/// A [`Refusal`] when no entry applies.
pub fn match_chip(found: &ChipIdentity) -> Result<&'static ChipEntry, Refusal> {
    let mut unmatchable = Vec::new();
    for entry in KNOWN_CHIPS {
        if entry.vendor != found.vendor {
            continue;
        }
        match entry.match_rule {
            MatchRule::X86FamilyModels { family, models } => {
                if found.family == family && models.contains(&found.model) {
                    return Ok(entry);
                }
            }
            MatchRule::X86FamilyRange { lo, hi } => {
                if found.family >= lo && found.family <= hi {
                    return Ok(entry);
                }
            }
            MatchRule::Midr { implementer, part } => match (implementer, part) {
                (
                    TableValue::Recorded { value: imp, .. },
                    TableValue::Recorded { value: prt, .. },
                ) => {
                    if found.midr_implementer() == imp && found.midr_part() == prt {
                        return Ok(entry);
                    }
                }
                _ => {
                    for reason in [implementer.absent_reason(), part.absent_reason()]
                        .into_iter()
                        .flatten()
                    {
                        unmatchable.push(format!("{}: {reason}", entry.name));
                    }
                }
            },
        }
    }
    unmatchable.sort();
    unmatchable.dedup();
    Err(Refusal {
        found: found.clone(),
        unmatchable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intel(model: u32) -> ChipIdentity {
        ChipIdentity {
            vendor: Vendor::GenuineIntel,
            family: 0x6,
            model,
            stepping: 0xc,
            midr: 0,
            microcode_rev: Some("0x00000000000000f8".to_string()),
        }
    }

    fn amd(family: u32) -> ChipIdentity {
        ChipIdentity {
            vendor: Vendor::AuthenticAMD,
            family,
            model: 0x31,
            stepping: 0,
            midr: 0,
            microcode_rev: None,
        }
    }

    #[test]
    fn the_table_carries_the_three_specified_entries_with_their_event_configs() {
        assert_eq!(KNOWN_CHIPS.len(), 3);
        assert_eq!(INTEL_06_9E.work_clock_config.value(), Some(0x1c4));
        assert_eq!(AMD_ZEN.work_clock_config.value(), Some(0x0051_00d1));
        assert_eq!(NEOVERSE_N1.work_clock_config.value(), Some(0x21));
    }

    #[test]
    fn every_entry_names_a_pmu_shape_and_at_least_one_host_condition() {
        for entry in KNOWN_CHIPS {
            assert!(
                !entry.host_conditions.is_empty(),
                "{} names no host conditions",
                entry.name
            );
            // A table row with no work-clock event would select no measurement.
            assert!(!entry.work_clock_event.is_empty(), "{}", entry.name);
            match entry.pmu_shape {
                PmuShape::IntelArchPerfmon { version } => assert!(version > 0),
                PmuShape::AmdCore => {}
                PmuShape::ArmPmuV3 { event_id_register } => {
                    assert!(!event_id_register.is_empty());
                }
            }
        }
    }

    #[test]
    fn a_departure_from_rr_carries_a_reason_and_a_match_does_not() {
        assert!(
            INTEL_06_9E.departure.is_some(),
            "0x1c4 differs from 0x5101c4"
        );
        assert!(AMD_ZEN.departure.is_none(), "0x5100d1 is rr's own value");
        assert!(NEOVERSE_N1.departure.is_none(), "0x21 is rr's own value");
        for entry in KNOWN_CHIPS {
            if let Some(rr) = entry.rr_reference {
                let same = entry.work_clock_config.value()
                    == u64::from_str_radix(rr.trim_start_matches("0x"), 16).ok();
                assert_eq!(
                    same,
                    entry.departure.is_none(),
                    "{} must record a reason exactly when it departs from rr",
                    entry.name
                );
            }
        }
    }

    #[test]
    fn the_intel_entry_matches_its_family_and_model_and_ignores_stepping() {
        let entry = match_chip(&intel(0x9e)).expect("06_9EH is in the table");
        assert_eq!(entry.name, "GenuineIntel 06_9EH");
        let mut other_stepping = intel(0x9e);
        other_stepping.stepping = 0xa;
        assert_eq!(
            match_chip(&other_stepping)
                .expect("stepping is not part of the rule")
                .name,
            entry.name
        );
    }

    #[test]
    fn the_amd_entry_covers_families_17h_through_1ah_and_nothing_outside() {
        for family in 0x17..=0x1a {
            assert_eq!(
                match_chip(&amd(family)).expect("in range").name,
                "AuthenticAMD families 17h-1Ah",
                "family {family:#x}"
            );
        }
        for family in [0x15, 0x16, 0x1b, 0x1f] {
            assert!(
                match_chip(&amd(family)).is_err(),
                "family {family:#x} must not match"
            );
        }
    }

    #[test]
    fn an_unmatched_chip_is_refused_with_what_was_found() {
        let found = intel(0x55);
        let refusal = match_chip(&found).expect_err("06_55H is not in the table");
        assert_eq!(refusal.found, found);
        assert_eq!(refusal.found.family_model_stepping(), "06_55_0c");
    }

    #[test]
    fn an_entry_whose_match_rule_has_no_recorded_value_never_matches_and_says_why() {
        // Any aarch64 identity at all: the Neoverse N1 rule rests on values no
        // source records, so it must refuse rather than match something.
        let found = ChipIdentity {
            vendor: Vendor::Aarch64,
            family: 0,
            model: 0,
            stepping: 0,
            midr: 0x413f_d0c0,
            microcode_rev: None,
        };
        let refusal = match_chip(&found).expect_err("the N1 rule cannot match");
        assert_eq!(refusal.unmatchable.len(), 2, "{:?}", refusal.unmatchable);
        assert!(
            refusal
                .unmatchable
                .iter()
                .all(|line| line.starts_with("Neoverse N1: ")),
            "{:?}",
            refusal.unmatchable
        );
        assert_eq!(found.midr_implementer(), 0x41);
        assert_eq!(found.midr_part(), 0xd0c);
    }

    #[test]
    fn the_amd_lock_probe_event_is_rrs_and_no_other_entry_has_one() {
        let probe = AMD_ZEN.lock_probe_event.expect("AMD needs the lock probe");
        assert_eq!(probe.value(), Some(0x0051_0825));
        assert!(
            matches!(probe, TableValue::Recorded { source, .. } if source == "rr src/PerfCounters_x86.h check_for_zen_speclockmap")
        );
        assert!(INTEL_06_9E.lock_probe_event.is_none());
        assert!(NEOVERSE_N1.lock_probe_event.is_none());
    }

    #[test]
    fn amd_requires_the_three_conditions_intel_does_not() {
        for extra in [
            HostConditionKind::SpecLockMapDisabled,
            HostConditionKind::SsbMitigationPinned,
            HostConditionKind::AvicOff,
        ] {
            assert!(AMD_ZEN.host_conditions.contains(&extra), "{extra:?}");
            assert!(!INTEL_06_9E.host_conditions.contains(&extra), "{extra:?}");
        }
        for shared in [
            HostConditionKind::NmiWatchdogOff,
            HostConditionKind::GovernorPinned,
            HostConditionKind::SmtPolicy,
        ] {
            assert!(INTEL_06_9E.host_conditions.contains(&shared), "{shared:?}");
            assert!(AMD_ZEN.host_conditions.contains(&shared), "{shared:?}");
        }
    }

    #[test]
    fn contract_columns_are_the_intel_file_the_amd_draft_and_nothing_for_arm() {
        assert_eq!(INTEL_06_9E.contract, Some("docs/cpu-msr-contract.toml"));
        assert_eq!(
            AMD_ZEN.contract,
            Some("docs/cpu-msr-contract-amd-draft.toml")
        );
        assert_eq!(NEOVERSE_N1.contract, None);
    }

    #[test]
    fn condition_tokens_are_distinct_and_kebab_case() {
        let mut tokens: Vec<&str> = [
            HostConditionKind::NmiWatchdogOff,
            HostConditionKind::GovernorPinned,
            HostConditionKind::SmtPolicy,
            HostConditionKind::KvmPresent,
            HostConditionKind::KvmModuleIdentity,
            HostConditionKind::CorePinning,
            HostConditionKind::SpecLockMapDisabled,
            HostConditionKind::SsbMitigationPinned,
            HostConditionKind::AvicOff,
        ]
        .iter()
        .map(|k| k.token())
        .collect();
        let count = tokens.len();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(tokens.len(), count, "condition tokens must be distinct");
        assert!(
            tokens
                .iter()
                .all(|t| t.chars().all(|c| c.is_ascii_lowercase() || c == '-')),
            "{tokens:?}"
        );
    }

    #[test]
    fn vendor_cpuid_strings_are_the_leaf_zero_spellings() {
        assert_eq!(Vendor::GenuineIntel.cpuid_string(), Some("GenuineIntel"));
        assert_eq!(Vendor::AuthenticAMD.cpuid_string(), Some("AuthenticAMD"));
        assert_eq!(Vendor::Aarch64.cpuid_string(), None);
    }
}
