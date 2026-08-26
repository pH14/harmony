// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stage 0 — host and chip check. The portable half.
//!
//! Stage 0 answers two questions: is this the chip we think it is, and is the host
//! in the required state. The reads themselves are Linux-only and live in
//! [`crate::stage0_sys`]; everything here — matching the chip against the table,
//! turning readings into expect-versus-found rows, and comparing two runs' rows —
//! is portable and unit-tested everywhere.
//!
//! A condition the pack records no expectation for, or that nothing read, is a
//! refusal. Neither is a row that quietly passes.

use crate::chips::{ChipEntry, ChipIdentity, HostConditionKind, Refusal};
use crate::dispositions::{Disposition, DispositionError};
use crate::pack::{HostConditionExpectation, Pack, PackError};
use crate::report::Record;

/// A refusal from stage 0.
#[derive(Debug, thiserror::Error)]
pub enum Stage0Error {
    /// The chip matched no entry in the known-chip table.
    #[error(
        "chip is not in the known-chip table: vendor {vendor:?}, identity {identity}; \
         entries that could not be matched: {unmatchable:?}"
    )]
    UnknownChip {
        /// The vendor string that was read.
        vendor: String,
        /// The chip identity that was read.
        identity: String,
        /// Table entries that share the vendor but rest on values no source
        /// records, and why.
        unmatchable: Vec<String>,
    },
    /// The pack could not be loaded or a field it holds could not be read.
    #[error(transparent)]
    Pack(#[from] PackError),
    /// The pack for this baseline is for a different chip than the one found.
    #[error("pack {baseline} is for {expect}, but this host is {found}")]
    WrongChip {
        /// The baseline named on the command line.
        baseline: String,
        /// The identity the pack records.
        expect: String,
        /// The identity that was read.
        found: String,
    },
    /// The pack records no expected state for a condition the table requires,
    /// at the place it was read.
    #[error("pack records no expected state for required condition {condition} at {scope}")]
    NoExpectation {
        /// The condition's token.
        condition: String,
        /// Where the condition was read.
        scope: String,
    },
    /// Nothing read a condition the table requires.
    #[error("nothing read required condition {condition}; stage 0 cannot confirm it")]
    NoReading {
        /// The condition's token.
        condition: String,
    },
    /// The host cannot be read from here.
    #[error("stage 0 reads Linux host state and this build is for {target}")]
    WrongPlatform {
        /// The platform this build targets.
        target: &'static str,
    },
    /// A read that must succeed did not.
    #[error("cannot read {what}: {detail}")]
    Read {
        /// What was being read.
        what: String,
        /// Why the read failed.
        detail: String,
    },
    /// A probe the chip's entry requires cannot run, because the table records
    /// no value for it.
    #[error("{probe} cannot run: {reason}")]
    ProbeUnavailable {
        /// What the probe checks.
        probe: String,
        /// Why the table records no value for it.
        reason: String,
    },
    /// The work-clock event did not open the way the chip's entry requires.
    #[error("work-clock event {config:#x} is not usable: {detail}")]
    WorkClock {
        /// The event config that was opened.
        config: u64,
        /// What was wrong with it.
        detail: String,
    },
}

impl From<Refusal> for Stage0Error {
    fn from(refusal: Refusal) -> Stage0Error {
        Stage0Error::UnknownChip {
            vendor: format!("{:?}", refusal.found.vendor),
            identity: refusal.found.family_model_stepping(),
            unmatchable: refusal.unmatchable,
        }
    }
}

/// One raw host reading: what a condition was found to be, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// Which condition this reads.
    pub condition: HostConditionKind,
    /// Where it was read: `host`, or a per-core name such as `cpu3`.
    pub scope: String,
    /// What was read.
    pub found: String,
}

impl Reading {
    /// A reading of `condition` at `scope`.
    pub fn new(
        condition: HostConditionKind,
        scope: impl Into<String>,
        found: impl Into<String>,
    ) -> Reading {
        Reading {
            condition,
            scope: scope.into(),
            found: found.into(),
        }
    }
}

/// One expect-versus-found row. A favorable deviation is still a deviation, so a
/// row that is not confirmed carries no verdict of its own — it carries whatever
/// disposition a person recorded, and nothing until then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The condition's token.
    pub condition: String,
    /// Where the condition was read.
    pub scope: String,
    /// What the pack says the state must be.
    pub expect: String,
    /// What was read.
    pub found: String,
    /// Whether the two agree.
    pub confirmed: bool,
    /// How a disagreement was dispositioned.
    pub disposition: Option<String>,
}

impl Row {
    /// A row comparing `found` against `expect`.
    pub fn new(
        condition: impl Into<String>,
        scope: impl Into<String>,
        expect: impl Into<String>,
        found: impl Into<String>,
    ) -> Row {
        let expect = expect.into();
        let found = found.into();
        Row {
            condition: condition.into(),
            scope: scope.into(),
            confirmed: expect.trim() == found.trim(),
            expect,
            found,
            disposition: None,
        }
    }

    /// The retained record for this row.
    #[must_use]
    pub fn to_record(&self) -> Record {
        Record::HostRow {
            condition: self.condition.clone(),
            scope: self.scope.clone(),
            expect: self.expect.clone(),
            found: self.found.clone(),
            confirmed: self.confirmed,
            disposition: self.disposition.clone(),
        }
    }
}

/// What opening the work-clock event on this chip showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkClockProbe {
    /// The config that was opened.
    pub config: u64,
    /// The count a known-nonzero payload produced.
    pub count: u64,
    /// Whether the counter was time-shared. A pinned counter must never be.
    pub multiplexed: bool,
    /// Whether the guest-only variant of the same event also opens. The work
    /// clock counts guest execution, so a chip that cannot filter to the guest
    /// cannot host the machinery.
    pub guest_only_opened: bool,
}

/// The rows comparing a work-clock probe against the pack.
///
/// # Errors
/// [`Stage0Error::WorkClock`] when the pack records no usable config,
/// [`PackError`] refusals when a field it needs is absent.
pub fn work_clock_rows(pack: &Pack, probe: &WorkClockProbe) -> Result<Vec<Row>, Stage0Error> {
    let expect_config = pack.work_clock.config()?;
    Ok(vec![
        Row::new(
            "work-clock-event-config",
            "host",
            format!("{expect_config:#x}"),
            format!("{:#x}", probe.config),
        ),
        Row::new(
            "work-clock-non-multiplexed",
            "host",
            "non-multiplexed",
            if probe.multiplexed {
                "multiplexed"
            } else {
                "non-multiplexed"
            },
        ),
        Row::new(
            "work-clock-counts",
            "host",
            "nonzero",
            if probe.count > 0 { "nonzero" } else { "zero" },
        ),
        Row::new(
            "work-clock-guest-only",
            "host",
            "openable",
            if probe.guest_only_opened {
                "openable"
            } else {
                "refused"
            },
        ),
    ])
}

/// Parse a `/sys` cpu list: comma-separated single numbers and inclusive
/// `lo-hi` ranges, such as `0-3,8,12-15`. Unparseable pieces are dropped rather
/// than guessed at; the caller refuses on an empty result.
#[must_use]
pub fn parse_cpu_list(text: &str) -> Vec<usize> {
    let mut cpus = Vec::new();
    for piece in text.trim().split(',') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        match piece.split_once('-') {
            Some((lo, hi)) => {
                if let (Ok(lo), Ok(hi)) = (lo.trim().parse::<usize>(), hi.trim().parse::<usize>())
                    && lo <= hi
                {
                    cpus.extend(lo..=hi);
                }
            }
            None => {
                if let Ok(cpu) = piece.parse::<usize>() {
                    cpus.push(cpu);
                }
            }
        }
    }
    cpus.sort_unstable();
    cpus.dedup();
    cpus
}

/// The first `processor` stanza of `/proc/cpuinfo`, as key-value pairs. Chip
/// identity is per-package, so the first stanza speaks for the chip.
#[must_use]
pub fn cpuinfo_first_stanza(text: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_string();
        if key == "processor" && !fields.is_empty() {
            break;
        }
        fields.push((key, value.trim().to_string()));
    }
    fields
}

/// One field of a `/proc/cpuinfo` stanza.
#[must_use]
pub fn cpuinfo_field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// A microcode or firmware revision in the sixteen-digit spelling the contract
/// and the pack use. Decimal and `0x` forms both parse; anything else is not a
/// revision and yields nothing rather than a guess.
#[must_use]
pub fn normalize_revision(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let parsed = match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok()?,
        None => raw.parse::<u64>().ok()?,
    };
    Some(format!("{parsed:#018x}"))
}

/// A kernel module parameter's `Y`/`N`/`0`/`1` spellings, as one word. A
/// spelling that is neither passes through, so an unexpected mode shows up in
/// the row rather than being folded into one of the two.
#[must_use]
pub fn normalize_bool(raw: &str) -> String {
    match raw.trim() {
        "Y" | "y" | "1" => "on".to_string(),
        "N" | "n" | "0" => "off".to_string(),
        other => other.to_string(),
    }
}

/// What stage 0 produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stage0Outcome {
    /// The table entry the chip matched.
    pub entry_name: String,
    /// What the chip reported about itself.
    pub chip: ChipIdentity,
    /// Every expect-versus-found row, sorted by condition then scope.
    pub rows: Vec<Row>,
}

impl Stage0Outcome {
    /// Mark every deviating row a recorded acceptance covers, and refuse if any
    /// acceptance covered nothing.
    ///
    /// # Errors
    /// [`DispositionError::Stale`] when an acceptance matched no deviating row.
    pub fn apply_dispositions(
        &mut self,
        dispositions: &[Disposition],
    ) -> Result<(), DispositionError> {
        let mut used = vec![false; dispositions.len()];
        for row in &mut self.rows {
            if row.confirmed {
                continue;
            }
            for (i, d) in dispositions.iter().enumerate() {
                if d.covers(&row.condition, &row.scope, &row.found) {
                    row.disposition = Some(d.why.clone());
                    used[i] = true;
                    break;
                }
            }
        }
        let stale: Vec<String> = dispositions
            .iter()
            .zip(&used)
            .filter(|(_, used)| !**used)
            .map(|(d, _)| d.describe())
            .collect();
        if stale.is_empty() {
            Ok(())
        } else {
            Err(DispositionError::Stale { stale })
        }
    }

    /// Rows that are neither confirmed nor dispositioned.
    #[must_use]
    pub fn deviations(&self) -> Vec<&Row> {
        self.rows
            .iter()
            .filter(|r| !r.confirmed && r.disposition.is_none())
            .collect()
    }

    /// The retained records for this outcome: the chip identity, then every row.
    #[must_use]
    pub fn to_records(&self) -> Vec<Record> {
        let mut records = vec![Record::ChipIdentity {
            vendor: format!("{:?}", self.chip.vendor),
            identity: self.identity_text(),
            microcode_rev: self.chip.microcode_rev.clone(),
            table_entry: self.entry_name.clone(),
        }];
        records.extend(self.rows.iter().map(Row::to_record));
        records
    }

    /// Add rows from a chip-specific probe, keeping the row order stable.
    pub fn add_rows(&mut self, extra: impl IntoIterator<Item = Row>) {
        self.rows.extend(extra);
        sort_rows(&mut self.rows);
    }

    /// The chip identity in the spelling the pack uses.
    #[must_use]
    pub fn identity_text(&self) -> String {
        identity_text(&self.chip)
    }
}

/// The chip identity in the spelling the pack uses: `family_model_stepping` on
/// x86, the raw `MIDR_EL1` value on aarch64.
#[must_use]
pub fn identity_text(chip: &ChipIdentity) -> String {
    if chip.midr == 0 {
        chip.family_model_stepping()
    } else {
        format!("{:#x}", chip.midr)
    }
}

/// Build the expect-versus-found rows for a chip, its table entry, its pack, and
/// the readings a host produced.
///
/// # Errors
/// [`Stage0Error::WrongChip`] when the pack is for different silicon,
/// [`Stage0Error::NoExpectation`] when the pack records no state for a required
/// condition, [`Stage0Error::NoReading`] when nothing read one.
pub fn build_rows(
    entry: &ChipEntry,
    pack: &Pack,
    chip: &ChipIdentity,
    readings: &[Reading],
    work_clock: &WorkClockProbe,
) -> Result<Stage0Outcome, Stage0Error> {
    let mut rows = work_clock_rows(pack, work_clock)?;

    // Identity first: a pack for the wrong silicon is not a comparison worth
    // continuing past.
    let expect_identity = pack.chip.identity.require("chip.identity")?;
    let found_identity = identity_text(chip);
    if expect_identity.trim() != found_identity.trim() {
        return Err(Stage0Error::WrongChip {
            baseline: pack.pack.baseline.clone(),
            expect: expect_identity.clone(),
            found: found_identity,
        });
    }
    rows.push(Row::new(
        "chip-identity",
        "host",
        expect_identity.clone(),
        found_identity,
    ));

    if let Some(expect_vendor) = pack.chip.vendor.value() {
        let found = entry.vendor.cpuid_string().unwrap_or("aarch64").to_string();
        rows.push(Row::new(
            "chip-vendor",
            "host",
            expect_vendor.clone(),
            found,
        ));
    }
    if let Some(expect_rev) = pack.chip.microcode_rev.value() {
        let found = chip
            .microcode_rev
            .clone()
            .unwrap_or_else(|| "unreadable".to_string());
        rows.push(Row::new(
            "chip-microcode-rev",
            "host",
            expect_rev.clone(),
            found,
        ));
    }

    // Every condition the entry requires needs both an expectation and a reading.
    let expectations = pack.host_conditions.require("host_conditions")?;
    for kind in entry.host_conditions {
        let token = kind.token();
        let for_condition: Vec<&HostConditionExpectation> = expectations
            .iter()
            .filter(|e| e.condition == token)
            .collect();
        let matching: Vec<&Reading> = readings.iter().filter(|r| r.condition == *kind).collect();
        if matching.is_empty() {
            return Err(Stage0Error::NoReading {
                condition: token.to_string(),
            });
        }
        for reading in matching {
            // A condition can be read in more than one place — one row per
            // online core, one per loaded KVM module — and those places do not
            // have to share a state. An expectation naming the reading's scope
            // wins; otherwise a lone expectation speaks for every place the
            // condition was read, and several that name other places do not.
            let expectation = for_condition
                .iter()
                .find(|e| e.scope == reading.scope)
                .or(match for_condition.as_slice() {
                    [only] => Some(only),
                    _ => None,
                })
                .ok_or_else(|| Stage0Error::NoExpectation {
                    condition: token.to_string(),
                    scope: reading.scope.clone(),
                })?;
            rows.push(Row::new(
                token,
                reading.scope.clone(),
                expectation.expect.clone(),
                reading.found.clone(),
            ));
        }
    }

    sort_rows(&mut rows);
    Ok(Stage0Outcome {
        entry_name: entry.name.to_string(),
        chip: chip.clone(),
        rows,
    })
}

fn sort_rows(rows: &mut [Row]) {
    rows.sort_by(|a, b| {
        (a.condition.as_str(), a.scope.as_str()).cmp(&(b.condition.as_str(), b.scope.as_str()))
    });
}

/// Where two runs' rows differ. A full qualification requires the rows to come
/// out identical across two reboots; this is what compares them.
#[must_use]
pub fn rows_differ(a: &[Row], b: &[Row]) -> Vec<String> {
    let mut differences = Vec::new();
    let key = |r: &Row| (r.condition.clone(), r.scope.clone());
    for row in a {
        match b.iter().find(|other| key(other) == key(row)) {
            Some(other) if other.found == row.found => {}
            Some(other) => differences.push(format!(
                "{}[{}]: first run found {:?}, second run found {:?}",
                row.condition, row.scope, row.found, other.found
            )),
            None => differences.push(format!(
                "{}[{}]: present in the first run, absent from the second",
                row.condition, row.scope
            )),
        }
    }
    for row in b {
        if !a.iter().any(|other| key(other) == key(row)) {
            differences.push(format!(
                "{}[{}]: absent from the first run, present in the second",
                row.condition, row.scope
            ));
        }
    }
    differences.sort();
    differences
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chips::Vendor;
    use crate::pack::{
        ChipSection, Field, HostConditionExpectation, Pack, PackHeader, SingleStepSection,
        SkidSection, WorkClockSection,
    };

    fn intel_chip() -> ChipIdentity {
        ChipIdentity {
            vendor: Vendor::GenuineIntel,
            family: 0x6,
            model: 0x9e,
            stepping: 0xc,
            midr: 0,
            microcode_rev: Some("0x00000000000000f8".to_string()),
        }
    }

    /// A pack whose host conditions are recorded, so the row builder has both
    /// halves of every comparison.
    fn pack_with_conditions(conditions: Vec<HostConditionExpectation>) -> Pack {
        let mut pack = Pack {
            pack: PackHeader {
                schema: crate::pack::PACK_SCHEMA.to_string(),
                baseline: "det-cfl-v1".to_string(),
                pack_hash: String::new(),
            },
            chip: ChipSection {
                vendor: Field::recorded("a/path", "GenuineIntel".to_string()),
                identity: Field::recorded("a/path", "06_9e_0c".to_string()),
                microcode_rev: Field::recorded("a/path", "0x00000000000000f8".to_string()),
            },
            work_clock: WorkClockSection {
                event_config: Field::recorded("a/path", "0x1c4".to_string()),
                event_name: Field::recorded("a/path", "AN_EVENT".to_string()),
                counting_scope: Field::recorded("a/path", "pinned".to_string()),
            },
            skid: SkidSection {
                observed_max: Field::absent("not measured"),
                margin: Field::recorded("a/path", 256),
                derivation: Field::absent("not recorded"),
                overshoot: Field::absent("not recorded"),
            },
            count_offsets: Field::absent("not measured"),
            event_density: Field::absent("not measured"),
            single_step: SingleStepSection {
                mechanism: Field::absent("not recorded"),
                work_per_step: Field::absent("not measured"),
            },
            host_conditions: Field::recorded("a/path", conditions),
        };
        pack.seal().expect("serializes");
        pack
    }

    fn expectation(kind: HostConditionKind, expect: &str, scope: &str) -> HostConditionExpectation {
        HostConditionExpectation {
            condition: kind.token().to_string(),
            expect: expect.to_string(),
            scope: scope.to_string(),
        }
    }

    fn intel_expectations() -> Vec<HostConditionExpectation> {
        vec![
            expectation(HostConditionKind::NmiWatchdogOff, "0", "host"),
            expectation(
                HostConditionKind::GovernorPinned,
                "performance",
                "every-core",
            ),
            expectation(HostConditionKind::SmtPolicy, "off", "host"),
            expectation(HostConditionKind::KvmPresent, "present", "host"),
            expectation(
                HostConditionKind::KvmModuleIdentity,
                "srcversion:abc",
                "host",
            ),
            expectation(HostConditionKind::CorePinning, "one-cpu", "host"),
        ]
    }

    fn intel_readings() -> Vec<Reading> {
        vec![
            Reading::new(HostConditionKind::NmiWatchdogOff, "host", "0"),
            Reading::new(HostConditionKind::GovernorPinned, "cpu0", "performance"),
            Reading::new(HostConditionKind::GovernorPinned, "cpu1", "performance"),
            Reading::new(HostConditionKind::SmtPolicy, "host", "off"),
            Reading::new(HostConditionKind::KvmPresent, "host", "present"),
            Reading::new(
                HostConditionKind::KvmModuleIdentity,
                "host",
                "srcversion:abc",
            ),
            Reading::new(HostConditionKind::CorePinning, "host", "one-cpu"),
        ]
    }

    fn intel_entry() -> &'static ChipEntry {
        crate::chips::match_chip(&intel_chip()).expect("06_9EH is in the table")
    }

    fn good_probe() -> WorkClockProbe {
        WorkClockProbe {
            config: 0x1c4,
            count: 1_000_000,
            multiplexed: false,
            guest_only_opened: true,
        }
    }

    #[test]
    fn a_matching_host_produces_confirmed_rows_for_every_required_condition() {
        let pack = pack_with_conditions(intel_expectations());
        let outcome = build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &intel_readings(),
            &good_probe(),
        )
        .expect("builds");
        assert!(outcome.deviations().is_empty(), "{:#?}", outcome.rows);
        // One row per required condition, plus the per-core governor rows and the
        // three identity rows.
        assert_eq!(outcome.rows.len(), 4 + 3 + 6 + 1);
        assert_eq!(outcome.entry_name, "GenuineIntel 06_9EH");
        // Rows are sorted, so two runs' output can be compared directly.
        let mut sorted = outcome.rows.clone();
        sorted.sort_by(|a, b| {
            (a.condition.as_str(), a.scope.as_str()).cmp(&(b.condition.as_str(), b.scope.as_str()))
        });
        assert_eq!(sorted, outcome.rows);
    }

    #[test]
    fn a_deviating_reading_is_a_row_that_is_not_confirmed() {
        let pack = pack_with_conditions(intel_expectations());
        let mut readings = intel_readings();
        readings[0] = Reading::new(HostConditionKind::NmiWatchdogOff, "host", "1");
        let outcome = build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &readings,
            &good_probe(),
        )
        .expect("builds");
        let deviations = outcome.deviations();
        assert_eq!(deviations.len(), 1);
        assert_eq!(deviations[0].condition, "nmi-watchdog-off");
        assert_eq!(deviations[0].expect, "0");
        assert_eq!(deviations[0].found, "1");
        assert!(deviations[0].disposition.is_none());
    }

    #[test]
    fn an_acceptance_naming_the_reading_disposes_of_the_deviation() {
        let pack = pack_with_conditions(intel_expectations());
        let mut readings = intel_readings();
        readings[0] = Reading::new(HostConditionKind::NmiWatchdogOff, "host", "1");
        let mut outcome = build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &readings,
            &good_probe(),
        )
        .expect("builds");
        outcome
            .apply_dispositions(&[Disposition {
                condition: "nmi-watchdog-off".to_string(),
                scope: None,
                found: "1".to_string(),
                why: "accepted here".to_string(),
            }])
            .expect("the acceptance matches");
        assert!(outcome.deviations().is_empty());
        let row = outcome
            .rows
            .iter()
            .find(|r| r.condition == "nmi-watchdog-off")
            .expect("the row is still there");
        assert!(!row.confirmed);
        assert_eq!(row.disposition.as_deref(), Some("accepted here"));
    }

    #[test]
    fn an_acceptance_naming_a_different_reading_leaves_the_deviation_live() {
        let pack = pack_with_conditions(intel_expectations());
        let mut readings = intel_readings();
        readings[0] = Reading::new(HostConditionKind::NmiWatchdogOff, "host", "1");
        let mut outcome = build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &readings,
            &good_probe(),
        )
        .expect("builds");
        let refusal = outcome.apply_dispositions(&[Disposition {
            condition: "nmi-watchdog-off".to_string(),
            scope: None,
            found: "2".to_string(),
            why: "accepted here".to_string(),
        }]);
        assert!(matches!(refusal, Err(DispositionError::Stale { .. })));
        assert_eq!(outcome.deviations().len(), 1);
    }

    #[test]
    fn an_acceptance_does_not_touch_a_confirmed_row() {
        let pack = pack_with_conditions(intel_expectations());
        let mut outcome = build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &intel_readings(),
            &good_probe(),
        )
        .expect("builds");
        let refusal = outcome.apply_dispositions(&[Disposition {
            condition: "nmi-watchdog-off".to_string(),
            scope: None,
            found: "0".to_string(),
            why: "accepted here".to_string(),
        }]);
        assert!(matches!(refusal, Err(DispositionError::Stale { .. })));
        assert!(outcome.rows.iter().all(|r| r.disposition.is_none()));
    }

    #[test]
    fn one_deviating_core_out_of_many_is_its_own_row() {
        let pack = pack_with_conditions(intel_expectations());
        let mut readings = intel_readings();
        readings.push(Reading::new(
            HostConditionKind::GovernorPinned,
            "cpu2",
            "powersave",
        ));
        let outcome = build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &readings,
            &good_probe(),
        )
        .expect("builds");
        let deviations = outcome.deviations();
        assert_eq!(deviations.len(), 1, "{:#?}", outcome.rows);
        assert_eq!(deviations[0].scope, "cpu2");
    }

    #[test]
    fn a_condition_with_no_expectation_is_refused_not_skipped() {
        let mut expectations = intel_expectations();
        expectations.retain(|e| e.condition != "smt-policy");
        let pack = pack_with_conditions(expectations);
        match build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &intel_readings(),
            &good_probe(),
        ) {
            Err(Stage0Error::NoExpectation { condition, scope }) => {
                assert_eq!(condition, "smt-policy");
                assert_eq!(scope, "host");
            }
            other => panic!("a missing expectation must refuse, got {other:?}"),
        }
    }

    #[test]
    fn a_condition_read_in_several_places_takes_the_expectation_naming_each() {
        let mut expectations = intel_expectations();
        expectations.retain(|e| e.condition != "kvm-module-identity");
        expectations.push(expectation(
            HostConditionKind::KvmModuleIdentity,
            "vendor module",
            "kvm_intel",
        ));
        expectations.push(expectation(
            HostConditionKind::KvmModuleIdentity,
            "shared module",
            "kvm",
        ));
        let mut readings = intel_readings();
        readings.retain(|r| r.condition != HostConditionKind::KvmModuleIdentity);
        readings.push(Reading::new(
            HostConditionKind::KvmModuleIdentity,
            "kvm_intel",
            "vendor module",
        ));
        readings.push(Reading::new(
            HostConditionKind::KvmModuleIdentity,
            "kvm",
            "shared module",
        ));
        let pack = pack_with_conditions(expectations);
        let outcome = build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &readings,
            &good_probe(),
        )
        .expect("each place has its own expectation");
        let rows: Vec<&Row> = outcome
            .rows
            .iter()
            .filter(|r| r.condition == "kvm-module-identity")
            .collect();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter().all(|r| r.confirmed),
            "one expectation per place must not be compared against the other place: {rows:?}"
        );
    }

    #[test]
    fn a_place_no_expectation_names_is_refused_rather_than_given_another_places_state() {
        let mut expectations = intel_expectations();
        expectations.retain(|e| e.condition != "kvm-module-identity");
        expectations.push(expectation(
            HostConditionKind::KvmModuleIdentity,
            "vendor module",
            "kvm_intel",
        ));
        expectations.push(expectation(
            HostConditionKind::KvmModuleIdentity,
            "shared module",
            "kvm",
        ));
        let mut readings = intel_readings();
        readings.retain(|r| r.condition != HostConditionKind::KvmModuleIdentity);
        readings.push(Reading::new(
            HostConditionKind::KvmModuleIdentity,
            "kvm_something_else",
            "a third module",
        ));
        let pack = pack_with_conditions(expectations);
        match build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &readings,
            &good_probe(),
        ) {
            Err(Stage0Error::NoExpectation { condition, scope }) => {
                assert_eq!(condition, "kvm-module-identity");
                assert_eq!(scope, "kvm_something_else");
            }
            other => panic!("an unnamed place must refuse, got {other:?}"),
        }
    }

    #[test]
    fn a_condition_nothing_read_is_refused_not_skipped() {
        let pack = pack_with_conditions(intel_expectations());
        let mut readings = intel_readings();
        readings.retain(|r| r.condition != HostConditionKind::KvmPresent);
        match build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &readings,
            &good_probe(),
        ) {
            Err(Stage0Error::NoReading { condition }) => assert_eq!(condition, "kvm-present"),
            other => panic!("a missing reading must refuse, got {other:?}"),
        }
    }

    #[test]
    fn a_pack_with_absent_host_conditions_refuses_rather_than_confirming_nothing() {
        let mut pack = pack_with_conditions(intel_expectations());
        pack.host_conditions = Field::absent("no recorded expected states");
        pack.seal().expect("serializes");
        match build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &intel_readings(),
            &good_probe(),
        ) {
            Err(Stage0Error::Pack(PackError::FieldAbsent { field, .. })) => {
                assert_eq!(field, "host_conditions");
            }
            other => panic!("an absent expectation set must refuse, got {other:?}"),
        }
    }

    #[test]
    fn a_pack_for_different_silicon_is_refused() {
        let pack = pack_with_conditions(intel_expectations());
        let mut other = intel_chip();
        other.stepping = 0xa;
        match build_rows(
            intel_entry(),
            &pack,
            &other,
            &intel_readings(),
            &good_probe(),
        ) {
            Err(Stage0Error::WrongChip {
                baseline,
                expect,
                found,
            }) => {
                assert_eq!(baseline, "det-cfl-v1");
                assert_eq!(expect, "06_9e_0c");
                assert_eq!(found, "06_9e_0a");
            }
            other => panic!("a pack for other silicon must refuse, got {other:?}"),
        }
    }

    #[test]
    fn identity_text_is_the_contract_spelling_on_x86_and_the_midr_on_aarch64() {
        assert_eq!(identity_text(&intel_chip()), "06_9e_0c");
        let arm = ChipIdentity {
            vendor: Vendor::Aarch64,
            family: 0,
            model: 0,
            stepping: 0,
            midr: 0x413f_d0c0,
            microcode_rev: None,
        };
        assert_eq!(identity_text(&arm), "0x413fd0c0");
    }

    #[test]
    fn rows_become_records_that_carry_the_whole_comparison() {
        let pack = pack_with_conditions(intel_expectations());
        let outcome = build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &intel_readings(),
            &good_probe(),
        )
        .expect("builds");
        let records = outcome.to_records();
        assert_eq!(records.len(), outcome.rows.len() + 1);
        match &records[0] {
            Record::ChipIdentity {
                identity,
                table_entry,
                ..
            } => {
                assert_eq!(identity, "06_9e_0c");
                assert_eq!(table_entry, "GenuineIntel 06_9EH");
            }
            other => panic!("the first record is the chip identity, got {other:?}"),
        }
        assert!(
            records[1..]
                .iter()
                .all(|r| matches!(r, Record::HostRow { .. }))
        );
    }

    #[test]
    fn two_runs_with_the_same_rows_differ_nowhere() {
        let pack = pack_with_conditions(intel_expectations());
        let first = build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &intel_readings(),
            &good_probe(),
        )
        .expect("builds")
        .rows;
        let second = first.clone();
        assert!(rows_differ(&first, &second).is_empty());
    }

    #[test]
    fn a_row_that_changed_between_runs_is_named_with_both_values() {
        let pack = pack_with_conditions(intel_expectations());
        let first = build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &intel_readings(),
            &good_probe(),
        )
        .expect("builds")
        .rows;
        let mut readings = intel_readings();
        readings[0] = Reading::new(HostConditionKind::NmiWatchdogOff, "host", "1");
        let second = build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &readings,
            &good_probe(),
        )
        .expect("builds")
        .rows;
        let differences = rows_differ(&first, &second);
        assert_eq!(differences.len(), 1, "{differences:?}");
        assert!(
            differences[0].contains("nmi-watchdog-off"),
            "{differences:?}"
        );
        assert!(differences[0].contains("\"0\""), "{differences:?}");
        assert!(differences[0].contains("\"1\""), "{differences:?}");
    }

    #[test]
    fn a_row_present_in_only_one_run_is_named() {
        let pack = pack_with_conditions(intel_expectations());
        let first = build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &intel_readings(),
            &good_probe(),
        )
        .expect("builds")
        .rows;
        let mut readings = intel_readings();
        readings.push(Reading::new(
            HostConditionKind::GovernorPinned,
            "cpu2",
            "performance",
        ));
        let second = build_rows(
            intel_entry(),
            &pack,
            &intel_chip(),
            &readings,
            &good_probe(),
        )
        .expect("builds")
        .rows;
        let differences = rows_differ(&first, &second);
        assert_eq!(differences.len(), 1, "{differences:?}");
        assert!(
            differences[0].contains("absent from the first run"),
            "{differences:?}"
        );
        // And symmetrically the other way round.
        let reverse = rows_differ(&second, &first);
        assert_eq!(reverse.len(), 1, "{reverse:?}");
        assert!(reverse[0].contains("absent from the second"), "{reverse:?}");
    }

    #[test]
    fn an_unknown_chip_becomes_a_refusal_naming_what_was_found() {
        let mut chip = intel_chip();
        chip.model = 0x55;
        let refusal = crate::chips::match_chip(&chip).expect_err("06_55H is not in the table");
        let error = Stage0Error::from(refusal);
        match error {
            Stage0Error::UnknownChip {
                vendor, identity, ..
            } => {
                assert_eq!(vendor, "GenuineIntel");
                assert_eq!(identity, "06_55_0c");
            }
            other => panic!("expected an unknown-chip refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_first_processor_stanza_is_the_one_read() {
        let text = "\
processor\t: 0
vendor_id\t: GenuineIntel
cpu family\t: 6
model\t\t: 158
stepping\t: 12
microcode\t: 0xf8

processor\t: 1
vendor_id\t: GenuineIntel
cpu family\t: 99
";
        let fields = cpuinfo_first_stanza(text);
        assert_eq!(cpuinfo_field(&fields, "cpu family"), Some("6"));
        assert_eq!(cpuinfo_field(&fields, "model"), Some("158"));
        assert_eq!(cpuinfo_field(&fields, "microcode"), Some("0xf8"));
        assert_eq!(cpuinfo_field(&fields, "no such field"), None);
    }

    #[test]
    fn a_revision_normalizes_to_the_spelling_the_contract_uses() {
        assert_eq!(
            normalize_revision("0xf8").as_deref(),
            Some("0x00000000000000f8")
        );
        assert_eq!(
            normalize_revision("248").as_deref(),
            Some("0x00000000000000f8")
        );
        assert_eq!(normalize_revision("not a number"), None);
    }

    #[test]
    fn a_module_parameter_spelling_the_table_does_not_know_passes_through() {
        assert_eq!(normalize_bool("Y"), "on");
        assert_eq!(normalize_bool("0"), "off");
        assert_eq!(normalize_bool("auto"), "auto");
    }
}
