// SPDX-License-Identifier: AGPL-3.0-or-later
//! The measured-constants pack: the per-chip data the VMM consumes.
//!
//! One pack file per chip baseline, checked in at `docs/chips/<baseline>.toml`,
//! embedded with `include_str!` and hash-pinned the same way
//! `docs/cpu-msr-contract.toml` is: the recorded `pack_hash` is the SHA-256 of the
//! canonical serialization with that field emptied, so a hand-edited pack fails to
//! load instead of silently changing what the VMM believes about the chip.
//!
//! Every value carries either the source it was transcribed or measured from, or an
//! explicit statement that no value is recorded. A pack never guesses: an absent
//! field is data, and a consumer that needs it refuses rather than substituting a
//! default.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The `[pack] schema` token this crate reads and writes.
pub const PACK_SCHEMA: &str = "cpu-qualification-pack-v1";

/// The `det-cfl-v1` pack, embedded at compile time. The path is relative to this
/// source file; moving the pack breaks the build loudly.
pub const DET_CFL_V1: &str = include_str!("../../../docs/chips/det-cfl-v1.toml");

/// The `det-zen3-v1` pack, embedded at compile time.
pub const DET_ZEN3_V1: &str = include_str!("../../../docs/chips/det-zen3-v1.toml");

/// The checked-in packs this build carries, by baseline name.
pub const BUILTIN_PACKS: &[(&str, &str)] =
    &[("det-cfl-v1", DET_CFL_V1), ("det-zen3-v1", DET_ZEN3_V1)];

/// The embedded pack text for `baseline`, if this build carries one.
#[must_use]
pub fn builtin_pack(baseline: &str) -> Option<&'static str> {
    BUILTIN_PACKS
        .iter()
        .find(|(name, _)| *name == baseline)
        .map(|(_, text)| *text)
}

/// A refusal from loading or checking a pack.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// The file is not the TOML shape a pack has.
    #[error("pack does not parse: {0}")]
    Parse(#[from] toml::de::Error),
    /// The pack could not be re-serialized for hashing.
    #[error("pack does not serialize: {0}")]
    Serialize(#[from] toml::ser::Error),
    /// The `[pack] schema` token is not the one this crate reads.
    #[error("pack schema is {found:?}, expected {expected:?}")]
    Schema {
        /// The token the file declares.
        found: String,
        /// The token this crate reads.
        expected: &'static str,
    },
    /// The recorded `pack_hash` does not match the pack's own bytes.
    #[error("pack_hash is {recorded}, but the pack canonicalizes to {computed}")]
    HashMismatch {
        /// The hash the file records.
        recorded: String,
        /// The hash the file's own content produces.
        computed: String,
    },
    /// No pack is checked in for the requested baseline.
    #[error("no pack for baseline {0:?}")]
    UnknownBaseline(String),
    /// A field the caller needs carries no value.
    #[error("pack field {field} is absent: {reason}")]
    FieldAbsent {
        /// The field's dotted name.
        field: &'static str,
        /// What the pack says about why it has no value.
        reason: String,
    },
    /// A field carries a value the caller cannot read.
    #[error("pack field {field} holds {value:?}, which is not {expected}")]
    FieldMalformed {
        /// The field's dotted name.
        field: &'static str,
        /// The recorded text.
        value: String,
        /// What the caller needed it to be.
        expected: &'static str,
    },
}

/// One pack value: recorded with its source, or explicitly absent with the reason.
///
/// Both variants serialize as a table, so a section may hold a mix of recorded and
/// absent fields without the emitted TOML depending on which is which.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Field<T> {
    /// A value, and where it came from — a repository path for a transcribed
    /// constant, or the stage and run that measured it.
    Recorded {
        /// Where the value came from.
        source: String,
        /// The value.
        value: T,
    },
    /// No value. The pack states why rather than carrying a guess.
    Absent {
        /// Why no value is recorded.
        absent: String,
    },
}

impl<T> Field<T> {
    /// A recorded value with its source.
    pub fn recorded(source: impl Into<String>, value: T) -> Self {
        Field::Recorded {
            source: source.into(),
            value,
        }
    }

    /// An absent value with the reason it is absent.
    pub fn absent(reason: impl Into<String>) -> Self {
        Field::Absent {
            absent: reason.into(),
        }
    }

    /// The value, if one is recorded.
    pub fn value(&self) -> Option<&T> {
        match self {
            Field::Recorded { value, .. } => Some(value),
            Field::Absent { .. } => None,
        }
    }

    /// Whether the field carries no value.
    pub fn is_absent(&self) -> bool {
        matches!(self, Field::Absent { .. })
    }

    /// The reason the field carries no value, if it carries none.
    pub fn absent_reason(&self) -> Option<&str> {
        match self {
            Field::Absent { absent } => Some(absent),
            Field::Recorded { .. } => None,
        }
    }

    /// The value, or a refusal naming the field and the pack's stated reason.
    ///
    /// # Errors
    /// [`PackError::FieldAbsent`] when the field carries no value.
    pub fn require(&self, field: &'static str) -> Result<&T, PackError> {
        match self {
            Field::Recorded { value, .. } => Ok(value),
            Field::Absent { absent } => Err(PackError::FieldAbsent {
                field,
                reason: absent.clone(),
            }),
        }
    }
}

/// The pack header.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackHeader {
    /// The format token; [`PACK_SCHEMA`] for packs this crate reads.
    pub schema: String,
    /// The chip baseline this pack describes, matching its filename and the
    /// `HostId` token the acceptance matrix uses.
    pub baseline: String,
    /// SHA-256 of the canonical serialization with this field emptied.
    pub pack_hash: String,
}

/// The chip this pack is for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChipSection {
    /// The CPUID leaf-0 vendor string on x86, or the implementer on aarch64.
    pub vendor: Field<String>,
    /// `family_model_stepping` in the `06_9e_0c` spelling on x86, or the MIDR
    /// value on aarch64.
    pub identity: Field<String>,
    /// The microcode or firmware revision the kernel records.
    pub microcode_rev: Field<String>,
}

/// The work clock on this chip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkClockSection {
    /// The `PERF_TYPE_RAW` config, as a `0x`-prefixed hex string.
    pub event_config: Field<String>,
    /// The vendor's name for the event.
    pub event_name: Field<String>,
    /// How the counter is opened: pinned, non-multiplexed, and what it counts.
    pub counting_scope: Field<String>,
}

impl WorkClockSection {
    /// The event config as a number.
    ///
    /// # Errors
    /// [`PackError::FieldAbsent`] when no config is recorded,
    /// [`PackError::FieldMalformed`] when the recorded text is not hex.
    pub fn config(&self) -> Result<u64, PackError> {
        let text = self.event_config.require("work_clock.event_config")?;
        parse_hex(text).ok_or_else(|| PackError::FieldMalformed {
            field: "work_clock.event_config",
            value: text.clone(),
            expected: "a 0x-prefixed hexadecimal event config",
        })
    }
}

/// Parse a `0x`-prefixed hexadecimal token.
fn parse_hex(text: &str) -> Option<u64> {
    let body = text
        .trim()
        .strip_prefix("0x")
        .or_else(|| text.trim().strip_prefix("0X"))?;
    u64::from_str_radix(body, 16).ok()
}

/// Overflow skid on this chip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkidSection {
    /// The largest skid stage 1 observed, in work units.
    pub observed_max: Field<u64>,
    /// The margin the machinery arms with, in work units.
    pub margin: Field<u64>,
    /// How the margin was derived from the observed distribution.
    pub derivation: Field<String>,
    /// What the machinery does when the distribution's tail is exceeded.
    pub overshoot: Field<String>,
}

/// A count offset for one exit class: the fixed number of work units an exit of
/// that class contributes on top of the guest's own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountOffset {
    /// The exit class.
    pub exit_class: String,
    /// The offset, in work units.
    pub offset: i64,
}

/// The measured event density of one payload class: work-clock events per
/// iteration of its body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventDensity {
    /// The payload class.
    pub payload_class: String,
    /// Work-clock events per iteration.
    pub events_per_iteration: u64,
}

/// Single-step semantics on this chip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SingleStepSection {
    /// The facility a single step goes through.
    pub mechanism: Field<String>,
    /// How many work units one step contributes.
    pub work_per_step: Field<u64>,
}

/// One standing host condition and the state it must be in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostConditionExpectation {
    /// The condition's name, matching a [`crate::chips::HostConditionKind`] token.
    pub condition: String,
    /// The state the condition must be in.
    pub expect: String,
    /// Where the condition must hold: `host` or `every-core`.
    pub scope: String,
}

/// The measured-constants pack for one chip baseline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pack {
    /// The header.
    pub pack: PackHeader,
    /// Chip identity.
    pub chip: ChipSection,
    /// The work clock.
    pub work_clock: WorkClockSection,
    /// Overflow skid.
    pub skid: SkidSection,
    /// Count offsets per exit class.
    pub count_offsets: Field<Vec<CountOffset>>,
    /// Event density per payload class.
    pub event_density: Field<Vec<EventDensity>>,
    /// Single-step semantics.
    pub single_step: SingleStepSection,
    /// The standing host conditions, and the state each must be in.
    pub host_conditions: Field<Vec<HostConditionExpectation>>,
}

impl Pack {
    /// Parse a pack, check its schema token, and check its recorded hash against
    /// its own bytes.
    ///
    /// # Errors
    /// [`PackError::Parse`] on a malformed file, [`PackError::Schema`] on an
    /// unreadable format token, [`PackError::HashMismatch`] when the recorded hash
    /// does not match the content.
    pub fn parse(text: &str) -> Result<Pack, PackError> {
        let pack = Pack::parse_unsealed(text)?;
        let computed = pack.compute_hash()?;
        if computed != pack.pack.pack_hash {
            return Err(PackError::HashMismatch {
                recorded: pack.pack.pack_hash.clone(),
                computed,
            });
        }
        Ok(pack)
    }

    /// Parse a pack and check its schema token, without checking its recorded
    /// hash. This is what sealing reads with: a pack being resealed is one whose
    /// recorded hash is stale by construction.
    ///
    /// # Errors
    /// [`PackError::Parse`] on a malformed file, [`PackError::Schema`] on an
    /// unreadable format token.
    pub fn parse_unsealed(text: &str) -> Result<Pack, PackError> {
        let pack: Pack = toml::from_str(text)?;
        if pack.pack.schema != PACK_SCHEMA {
            return Err(PackError::Schema {
                found: pack.pack.schema.clone(),
                expected: PACK_SCHEMA,
            });
        }
        Ok(pack)
    }

    /// Load the checked-in pack for `baseline`.
    ///
    /// # Errors
    /// [`PackError::UnknownBaseline`] when no pack is embedded for that name, or
    /// any [`Pack::parse`] refusal.
    pub fn builtin(baseline: &str) -> Result<Pack, PackError> {
        let text = builtin_pack(baseline)
            .ok_or_else(|| PackError::UnknownBaseline(baseline.to_string()))?;
        Pack::parse(text)
    }

    /// The canonical serialization: this pack with `pack_hash` emptied. Field
    /// order is the declaration order above and every collection is a `Vec`, so
    /// the bytes are a function of the content alone.
    ///
    /// # Errors
    /// [`PackError::Serialize`] when the pack cannot be written as TOML.
    pub fn canonical(&self) -> Result<String, PackError> {
        let mut bare = self.clone();
        bare.pack.pack_hash = String::new();
        Ok(toml::to_string(&bare)?)
    }

    /// SHA-256 of [`Pack::canonical`], lowercase hex.
    ///
    /// # Errors
    /// [`PackError::Serialize`] when the pack cannot be written as TOML.
    pub fn compute_hash(&self) -> Result<String, PackError> {
        let mut hasher = Sha256::new();
        hasher.update(self.canonical()?.as_bytes());
        Ok(hex(&hasher.finalize()))
    }

    /// Write `pack_hash` from the pack's own content.
    ///
    /// # Errors
    /// [`PackError::Serialize`] when the pack cannot be written as TOML.
    pub fn seal(&mut self) -> Result<(), PackError> {
        self.pack.pack_hash = self.compute_hash()?;
        Ok(())
    }

    /// Every field this pack leaves absent, as dotted names paired with the
    /// pack's stated reason. Sorted by name so a report is stable.
    #[must_use]
    pub fn absent_fields(&self) -> Vec<(&'static str, &str)> {
        let mut rows: Vec<(&'static str, &str)> = Vec::new();
        for (name, reason) in [
            ("chip.vendor", self.chip.vendor.absent_reason()),
            ("chip.identity", self.chip.identity.absent_reason()),
            (
                "chip.microcode_rev",
                self.chip.microcode_rev.absent_reason(),
            ),
            (
                "work_clock.event_config",
                self.work_clock.event_config.absent_reason(),
            ),
            (
                "work_clock.event_name",
                self.work_clock.event_name.absent_reason(),
            ),
            (
                "work_clock.counting_scope",
                self.work_clock.counting_scope.absent_reason(),
            ),
            ("skid.observed_max", self.skid.observed_max.absent_reason()),
            ("skid.margin", self.skid.margin.absent_reason()),
            ("skid.derivation", self.skid.derivation.absent_reason()),
            ("skid.overshoot", self.skid.overshoot.absent_reason()),
            ("count_offsets", self.count_offsets.absent_reason()),
            ("event_density", self.event_density.absent_reason()),
            (
                "single_step.mechanism",
                self.single_step.mechanism.absent_reason(),
            ),
            (
                "single_step.work_per_step",
                self.single_step.work_per_step.absent_reason(),
            ),
            ("host_conditions", self.host_conditions.absent_reason()),
        ] {
            if let Some(reason) = reason {
                rows.push((name, reason));
            }
        }
        rows.sort_unstable_by_key(|(name, _)| *name);
        rows
    }
}

/// Lowercase hex of a byte string.
fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        // Infallible: writing to a String never fails.
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pack with one recorded and one absent field in the same section, so the
    /// round trip exercises both encodings side by side.
    fn sample() -> Pack {
        let mut pack = Pack {
            pack: PackHeader {
                schema: PACK_SCHEMA.to_string(),
                baseline: "sample-v1".to_string(),
                pack_hash: String::new(),
            },
            chip: ChipSection {
                vendor: Field::recorded("a/path", "GenuineIntel".to_string()),
                identity: Field::recorded("a/path", "06_9e_0c".to_string()),
                microcode_rev: Field::absent("not read on this chip"),
            },
            work_clock: WorkClockSection {
                event_config: Field::recorded("a/path", "0x1c4".to_string()),
                event_name: Field::recorded("a/path", "AN_EVENT".to_string()),
                counting_scope: Field::absent("not recorded"),
            },
            skid: SkidSection {
                observed_max: Field::absent("not measured"),
                margin: Field::recorded("a/path", 256),
                derivation: Field::recorded("a/path", "twice the bound".to_string()),
                overshoot: Field::absent("not recorded"),
            },
            count_offsets: Field::recorded(
                "a/path",
                vec![CountOffset {
                    exit_class: "io".to_string(),
                    offset: -1,
                }],
            ),
            event_density: Field::absent("not measured"),
            single_step: SingleStepSection {
                mechanism: Field::recorded("a/path", "a facility".to_string()),
                work_per_step: Field::absent("not measured"),
            },
            host_conditions: Field::recorded(
                "a/path",
                vec![HostConditionExpectation {
                    condition: "nmi-watchdog-off".to_string(),
                    expect: "0".to_string(),
                    scope: "host".to_string(),
                }],
            ),
        };
        pack.seal().expect("a sample pack serializes");
        pack
    }

    #[test]
    fn a_pack_round_trips_through_toml_with_absent_and_recorded_fields() {
        let pack = sample();
        let text = toml::to_string(&pack).expect("serializes");
        let back: Pack = toml::from_str(&text).expect("parses");
        assert_eq!(back, pack);
        // Both encodings survive: the absent marker stays a reason, not a default.
        assert_eq!(back.skid.observed_max.absent_reason(), Some("not measured"));
        assert_eq!(back.skid.margin.value(), Some(&256));
    }

    #[test]
    fn parse_checks_the_recorded_hash() {
        let pack = sample();
        let text = toml::to_string(&pack).expect("serializes");
        let parsed = Pack::parse(&text).expect("a sealed pack parses");
        assert_eq!(parsed, pack);

        // Change one value without resealing: the hash no longer matches.
        let tampered = text.replace("value = 256", "value = 1024");
        assert_ne!(tampered, text, "the substitution must actually apply");
        match Pack::parse(&tampered) {
            Err(PackError::HashMismatch { recorded, computed }) => {
                assert_eq!(recorded, pack.pack.pack_hash);
                assert_ne!(computed, recorded);
            }
            other => panic!("a tampered pack must be refused, got {other:?}"),
        }
    }

    #[test]
    fn parse_refuses_an_unreadable_schema_token() {
        let mut pack = sample();
        pack.pack.schema = "cpu-qualification-pack-v99".to_string();
        pack.seal().expect("serializes");
        let text = toml::to_string(&pack).expect("serializes");
        match Pack::parse(&text) {
            Err(PackError::Schema { found, expected }) => {
                assert_eq!(found, "cpu-qualification-pack-v99");
                assert_eq!(expected, PACK_SCHEMA);
            }
            other => panic!("an unreadable schema must be refused, got {other:?}"),
        }
    }

    #[test]
    fn canonical_bytes_ignore_the_recorded_hash() {
        let mut a = sample();
        let before = a.canonical().expect("serializes");
        a.pack.pack_hash = "0".repeat(64);
        assert_eq!(a.canonical().expect("serializes"), before);
        assert_eq!(a.compute_hash().expect("hashes"), sample().pack.pack_hash);
    }

    #[test]
    fn require_names_the_field_and_repeats_the_stated_reason() {
        let pack = sample();
        match pack.skid.observed_max.require("skid.observed_max") {
            Err(PackError::FieldAbsent { field, reason }) => {
                assert_eq!(field, "skid.observed_max");
                assert_eq!(reason, "not measured");
            }
            other => panic!("an absent field must refuse, got {other:?}"),
        }
        assert_eq!(
            pack.skid.margin.require("skid.margin").expect("recorded"),
            &256
        );
    }

    #[test]
    fn work_clock_config_parses_hex_and_refuses_anything_else() {
        let pack = sample();
        assert_eq!(pack.work_clock.config().expect("recorded"), 0x1c4);

        let mut bad = sample();
        bad.work_clock.event_config = Field::recorded("a/path", "1c4".to_string());
        match bad.work_clock.config() {
            Err(PackError::FieldMalformed { field, value, .. }) => {
                assert_eq!(field, "work_clock.event_config");
                assert_eq!(value, "1c4");
            }
            other => panic!("a non-hex config must refuse, got {other:?}"),
        }
    }

    #[test]
    fn absent_fields_lists_every_absence_with_its_reason() {
        let pack = sample();
        let absent = pack.absent_fields();
        let names: Vec<&str> = absent.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            vec![
                "chip.microcode_rev",
                "event_density",
                "single_step.work_per_step",
                "skid.observed_max",
                "skid.overshoot",
                "work_clock.counting_scope",
            ]
        );
        assert!(absent.iter().all(|(_, reason)| !reason.is_empty()));
    }

    #[test]
    fn parse_hex_reads_both_prefix_spellings_and_rejects_the_rest() {
        assert_eq!(parse_hex("0x1c4"), Some(0x1c4));
        assert_eq!(parse_hex("0X5100D1"), Some(0x0051_00d1));
        assert_eq!(parse_hex(" 0x21 "), Some(0x21));
        assert_eq!(parse_hex("21"), None);
        assert_eq!(parse_hex("0xzz"), None);
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn hex_is_lowercase_and_two_digits_per_byte() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(hex(&[]), "");
    }

    #[test]
    fn the_embedded_det_cfl_v1_pack_loads_and_its_hash_holds() {
        let pack = Pack::builtin("det-cfl-v1").expect("the embedded pack must load");
        assert_eq!(pack.pack.baseline, "det-cfl-v1");
        assert_eq!(pack.pack.schema, PACK_SCHEMA);
        assert_eq!(
            pack.compute_hash().expect("hashes"),
            pack.pack.pack_hash,
            "the checked-in pack_hash must match the pack's own bytes"
        );
    }

    #[test]
    fn the_det_cfl_v1_pack_carries_the_transcribed_constants() {
        let pack = Pack::builtin("det-cfl-v1").expect("the embedded pack must load");
        assert_eq!(pack.work_clock.config().expect("recorded"), 0x1c4);
        assert_eq!(pack.skid.margin.value(), Some(&256));
        assert_eq!(
            pack.chip.identity.value().map(String::as_str),
            Some("06_9e_0c")
        );
        assert_eq!(
            pack.chip.vendor.value().map(String::as_str),
            Some("GenuineIntel")
        );
        // Every recorded value names the file it was transcribed from.
        for (field, source) in [
            ("chip.vendor", &pack.chip.vendor),
            ("chip.identity", &pack.chip.identity),
            ("chip.microcode_rev", &pack.chip.microcode_rev),
        ] {
            if let Field::Recorded { source, .. } = source {
                assert!(source.contains('/'), "{field} source is {source:?}");
            }
        }
    }

    #[test]
    fn the_det_cfl_v1_pack_marks_the_unmeasured_fields_absent_with_reasons() {
        let pack = Pack::builtin("det-cfl-v1").expect("the embedded pack must load");
        let absent: Vec<&str> = pack.absent_fields().iter().map(|(n, _)| *n).collect();
        assert_eq!(
            absent,
            vec![
                "count_offsets",
                "event_density",
                "host_conditions",
                "single_step.work_per_step",
                "skid.observed_max",
            ],
            "a field with no recorded source must be absent, not guessed"
        );
        for (name, reason) in pack.absent_fields() {
            assert!(
                reason.len() > 20,
                "{name} must say why it has no value, got {reason:?}"
            );
        }
    }

    #[test]
    fn the_embedded_det_zen3_v1_pack_loads_and_its_hash_holds() {
        let pack = Pack::builtin("det-zen3-v1").expect("the embedded pack must load");
        assert_eq!(pack.pack.baseline, "det-zen3-v1");
        assert_eq!(pack.pack.schema, PACK_SCHEMA);
        assert_eq!(
            pack.compute_hash().expect("hashes"),
            pack.pack.pack_hash,
            "the checked-in pack_hash must match the pack's own bytes"
        );
    }

    #[test]
    fn the_det_zen3_v1_pack_carries_the_event_this_program_pinned() {
        let pack = Pack::builtin("det-zen3-v1").expect("the embedded pack must load");
        assert_eq!(pack.work_clock.config().expect("recorded"), 0x0051_00d1);
        assert_eq!(
            pack.chip.vendor.value().map(String::as_str),
            Some("AuthenticAMD")
        );
        assert_eq!(
            pack.chip.identity.value().map(String::as_str),
            Some("19_01_01")
        );
        let conditions = pack
            .host_conditions
            .value()
            .expect("the AMD entry's conditions are recorded");
        // The conditions the AMD entry adds to the Intel list.
        for token in [
            "spec-lock-map-disabled",
            "ssb-mitigation-pinned",
            "avic-off",
            "perf-sample-ceiling",
        ] {
            assert!(
                conditions.iter().any(|c| c.condition == token),
                "{token} must carry an expectation"
            );
        }
    }

    #[test]
    fn the_sampling_ceiling_is_stated_for_both_knobs_that_suppress_an_overflow() {
        let pack = Pack::builtin("det-zen3-v1").expect("the embedded pack must load");
        let conditions = pack
            .host_conditions
            .value()
            .expect("the AMD entry's conditions are recorded");
        // Stage 1 arms far above the stock per-tick ceiling, and an overflow the
        // kernel suppresses is an arm the run cannot account for. Both knobs are
        // stated so a later run on this baseline cannot measure under the stock
        // one without stage 0 saying so.
        let ceiling: Vec<&HostConditionExpectation> = conditions
            .iter()
            .filter(|c| c.condition == "perf-sample-ceiling")
            .collect();
        assert_eq!(ceiling.len(), 2, "{ceiling:?}");
        for (scope, expect) in [
            ("max-sample-rate", "100000000"),
            ("cpu-time-max-percent", "0"),
        ] {
            assert!(
                ceiling
                    .iter()
                    .any(|c| c.scope == scope && c.expect == expect),
                "{scope} must expect {expect}, got {ceiling:?}"
            );
        }
    }

    #[test]
    fn the_det_zen3_v1_pack_marks_the_unmeasured_fields_absent_with_reasons() {
        let pack = Pack::builtin("det-zen3-v1").expect("the embedded pack must load");
        let absent: Vec<&str> = pack.absent_fields().iter().map(|(n, _)| *n).collect();
        assert_eq!(
            absent,
            vec!["count_offsets", "single_step.work_per_step"],
            "a field no run has measured must be absent, not guessed"
        );
        for (name, reason) in pack.absent_fields() {
            assert!(
                reason.len() > 20,
                "{name} must say why it has no value, got {reason:?}"
            );
        }
    }

    #[test]
    fn an_unknown_baseline_has_no_embedded_pack() {
        assert!(builtin_pack("no-such-chip").is_none());
        match Pack::builtin("no-such-chip") {
            Err(PackError::UnknownBaseline(name)) => assert_eq!(name, "no-such-chip"),
            other => panic!("an unknown baseline must refuse, got {other:?}"),
        }
    }
}
