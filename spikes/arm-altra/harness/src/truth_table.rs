// SPDX-License-Identifier: AGPL-3.0-or-later
//! The AA-0 capability truth table — the complete, machine-readable, reboot-diffable
//! artifact `arm-spike probe` writes (`truth-table.json`, `schemas/truth-table.schema.json`).
//!
//! AA-0's deliverable is not a transient printout of eight host capabilities: it is this
//! object, byte-identical across two reboots, with all thirteen mandatory rows plus the
//! machine identity and the standing core-assignment topology. The struct shape mirrors the
//! schema field for field; the assembly ([`assemble`]) is pure logic over probed values, so
//! it is testable off the box, and the hardware reads that feed it live in `bin/arm_spike`.
//!
//! **Untested on silicon.** No truth table matching the schema has been produced by hardware;
//! the `expected` column is what `docs/ARM-ALTRA.md` predicts, and a prediction is not a
//! finding.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The schema version this emitter writes. A reader refuses a version it does not know.
pub const SCHEMA_VERSION: u32 = 1;

/// The complete truth table.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TruthTable {
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Who this machine is, from the silicon.
    pub identity: Identity,
    /// The standing core-assignment table for this box.
    pub topology: Topology,
    /// The expect-vs-found rows (≥13 mandatory).
    pub rows: Vec<Row>,
}

/// Who the machine is — read from the silicon, never the procurement order.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Identity {
    /// `MIDR_EL1`, raw.
    pub midr: u64,
    /// `MIDR_EL1.Implementer` (0x41 = Arm Ltd).
    pub implementer: u8,
    /// `MIDR_EL1.PartNum` (0xd0c = Neoverse N1).
    pub part_num: u16,
    /// `MIDR_EL1.Variant`.
    pub variant: u8,
    /// `MIDR_EL1.Revision`.
    pub revision: u8,
    /// The SoC part as the platform reports it — operator-supplied (not in any register).
    pub soc: String,
    /// Online cores.
    pub core_count: u32,
    /// `uname -r` of the running host kernel.
    pub host_kernel: String,
    /// Firmware versions, sorted key/value (a `BTreeMap`, never a hash map: diffed across
    /// reboots) — operator-supplied.
    pub firmware: BTreeMap<String, String>,
}

/// The standing core-assignment table (operator-supplied box config).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Topology {
    /// Housekeeping cores.
    pub housekeeping_cores: Vec<u32>,
    /// Measurement cores.
    pub measurement_cores: Vec<u32>,
    /// Guest cores.
    pub guest_cores: Vec<u32>,
    /// The cpufreq governor the box stands at.
    pub governor: String,
}

/// One expect-vs-found row.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Row {
    /// Stable row identifier.
    pub id: String,
    /// Which surface the row was read from (`id-register`/`pmu`/`perf`/`kvm`/`platform`).
    pub kind: String,
    /// The row in one sentence, with its architectural source.
    pub question: String,
    /// What `docs/ARM-ALTRA.md` predicts.
    pub expected: String,
    /// What the silicon said.
    pub found: String,
    /// The raw evidence behind `found` — never empty.
    pub raw: String,
    /// `true` exactly when `found == expected`.
    pub confirmed: bool,
    /// The explicit ruling a deviation demands; `null` for a confirmed row.
    pub disposition: Option<String>,
}

/// A probed capability's outcome.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Found {
    /// The capability is present.
    Present,
    /// The capability is absent.
    Absent,
    /// The probe could not run — never collapsed into "absent".
    Unprobed,
}

impl Found {
    fn token(self) -> &'static str {
        match self {
            Found::Present => "present",
            Found::Absent => "absent",
            Found::Unprobed => "unprobed",
        }
    }
}

/// One probed capability/id-register input to [`assemble`].
pub struct RowInput {
    /// Stable id (must be one of the schema's mandatory ids for the mandatory rows).
    pub id: &'static str,
    /// The surface.
    pub kind: &'static str,
    /// The one-sentence question.
    pub question: &'static str,
    /// The predicted value: `"present"`/`"absent"` for a normative row, or a free string.
    pub expected: String,
    /// The observed value.
    pub found: String,
    /// The raw evidence (register value, ioctl return, …) — must be non-empty.
    pub raw: String,
}

impl RowInput {
    /// A present/absent capability row.
    #[must_use]
    pub fn cap(
        id: &'static str,
        kind: &'static str,
        question: &'static str,
        expected: Found,
        found: Found,
        raw: String,
    ) -> RowInput {
        RowInput {
            id,
            kind,
            question,
            expected: expected.token().to_string(),
            found: found.token().to_string(),
            raw,
        }
    }
}

/// The disposition an UNRESOLVED deviation carries — a machine cannot invent a ruling, so a
/// deviation with no operator ruling is flagged here and gates AA-0 acceptance. A deviation
/// WITH a ruling (from the `--rulings` input) carries that ruling instead and is acceptable.
pub const UNRULED_DEVIATION: &str = "UNRULED deviation (found != expected): AA-0 acceptance requires an explicit recorded \
     ruling for this row (supply one via --rulings)";

/// Assemble the complete truth table from the probed inputs, identity, topology, and the
/// operator's recorded `rulings` (row-id → disposition).
///
/// A row is `confirmed` exactly when `found == expected` (disposition `null`). A deviation
/// with a ruling in `rulings` carries THAT ruling as its disposition — an acceptable, recorded
/// deviation, including a favourable one (ECV unexpectedly present). A deviation with no ruling
/// carries [`UNRULED_DEVIATION`] and gates acceptance ([`TruthTable::unresolved`]).
#[must_use]
pub fn assemble(
    identity: Identity,
    topology: Topology,
    rows: Vec<RowInput>,
    rulings: &BTreeMap<String, String>,
) -> TruthTable {
    let rows = rows
        .into_iter()
        .map(|r| {
            let confirmed = r.found == r.expected;
            // A ruling must be non-empty after trimming: `{"ecv": ""}` (or whitespace) is not
            // a disposition — it would violate the schema's `disposition` minLength and record
            // no actual ruling — so it is treated as UNRULED and gates acceptance.
            let ruling = rulings
                .get(r.id)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty());
            let disposition = if confirmed {
                None
            } else if let Some(ruling) = ruling {
                Some(ruling.to_string())
            } else {
                Some(UNRULED_DEVIATION.to_string())
            };
            Row {
                id: r.id.to_string(),
                kind: r.kind.to_string(),
                question: r.question.to_string(),
                expected: r.expected,
                found: r.found,
                raw: r.raw,
                confirmed,
                disposition,
            }
        })
        .collect();
    TruthTable {
        schema_version: SCHEMA_VERSION,
        identity,
        topology,
        rows,
    }
}

impl TruthTable {
    /// The ids of deviation rows with NO recorded ruling — the rows that gate AA-0 acceptance.
    /// A ruled deviation (even a favourable one) is acceptable; an unruled one is not.
    #[must_use]
    pub fn unresolved(&self) -> Vec<&str> {
        self.rows
            .iter()
            .filter(|r| !r.confirmed && r.disposition.as_deref() == Some(UNRULED_DEVIATION))
            .map(|r| r.id.as_str())
            .collect()
    }

    /// Whether AA-0 is acceptable: every row is either confirmed or a RULED deviation.
    #[must_use]
    pub fn all_resolved(&self) -> bool {
        self.unresolved().is_empty()
    }

    /// The ids of the deviation rows (confirmed == false), ruled or not.
    #[must_use]
    pub fn deviations(&self) -> Vec<&str> {
        self.rows
            .iter()
            .filter(|r| !r.confirmed)
            .map(|r| r.id.as_str())
            .collect()
    }

    /// The ways the assembled table VIOLATES `schemas/truth-table.schema.json`'s structural
    /// constraints — the `minLength`/`minimum`/`minItems`/`enum` rules that operator-supplied
    /// metadata or a short row set can break. [`assemble`] is pure logic over probed values, so
    /// it will happily serialize an empty `soc`/`governor` or a short row set; `probe` would
    /// then report success on a table that violates the canonical schema (it only inspects
    /// unresolved rows). Validating the complete emitted table here closes that. Returns an
    /// empty vec when the table conforms.
    ///
    /// This mirrors the schema's structural constraints rather than running a general JSON-Schema
    /// validator (none is on the dependency whitelist). The confirmed⇔`found==expected` relation
    /// is upheld by [`assemble`] and so is not re-checked.
    #[must_use]
    pub fn schema_violations(&self) -> Vec<String> {
        let mut v = Vec::new();
        // Returns a violation for an empty (schema minLength 1) string, or None. Returning
        // rather than capturing `v` keeps the per-field and per-row pushes borrow-clean.
        let empty = |field: &str, s: &str| -> Option<String> {
            s.trim()
                .is_empty()
                .then(|| format!("{field} is empty (schema minLength 1)"))
        };
        v.extend(empty("identity.soc", &self.identity.soc));
        v.extend(empty("identity.host_kernel", &self.identity.host_kernel));
        v.extend(empty("topology.governor", &self.topology.governor));
        if self.identity.core_count < 1 {
            v.push("identity.core_count is 0 (schema minimum 1)".to_string());
        }
        // The thirteen mandatory AA-0 facts (schema `rows.minItems`).
        if self.rows.len() < 13 {
            v.push(format!(
                "rows has {} entries, below the schema minItems of 13 (the mandatory AA-0 facts)",
                self.rows.len()
            ));
        }
        const KINDS: [&str; 5] = ["id-register", "pmu", "perf", "kvm", "platform"];
        for (i, r) in self.rows.iter().enumerate() {
            v.extend(empty(&format!("rows[{i}].id"), &r.id));
            v.extend(empty(&format!("rows[{i}].question"), &r.question));
            v.extend(empty(&format!("rows[{i}].expected"), &r.expected));
            v.extend(empty(&format!("rows[{i}].found"), &r.found));
            v.extend(empty(&format!("rows[{i}].raw"), &r.raw));
            if !KINDS.contains(&r.kind.as_str()) {
                v.push(format!(
                    "rows[{i}].kind {:?} is not one of the schema enum {KINDS:?}",
                    r.kind
                ));
            }
            // `disposition` is `null` on a confirmed row; when present it must be non-empty
            // (schema minLength 1) — a blank ruling is not a ruling.
            if let Some(d) = &r.disposition
                && d.trim().is_empty()
            {
                v.push(format!(
                    "rows[{i}].disposition is an empty string (schema minLength 1; use null for a \
                     confirmed row)"
                ));
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Identity {
        Identity {
            midr: 0x413f_d0c0,
            implementer: 0x41,
            part_num: 0xd0c,
            variant: 3,
            revision: 0,
            soc: "Ampere Altra Q80-30".into(),
            core_count: 80,
            host_kernel: "6.18.35-arm64-det".into(),
            firmware: BTreeMap::from([("smpmpro".into(), "2.10".into())]),
        }
    }

    fn topology() -> Topology {
        Topology {
            housekeeping_cores: vec![0],
            measurement_cores: vec![1, 2, 3],
            guest_cores: vec![1, 2, 3],
            governor: "performance".into(),
        }
    }

    /// A conforming 13-row table, to mutate into schema violations.
    fn conforming_table() -> TruthTable {
        let ids = [
            "ecv",
            "lse",
            "pmuver",
            "sve",
            "nested-virt",
            "br-retired-pmceid1",
            "perf-raw-0x21-pinned",
            "host-overflow-delivers",
            "dev-kvm",
            "kvm-mode",
            "kvm-cap-set-guest-debug",
            "vgicv3-creatable",
            "writable-id-registers",
        ];
        TruthTable {
            schema_version: SCHEMA_VERSION,
            identity: identity(),
            topology: topology(),
            rows: ids
                .iter()
                .map(|id| Row {
                    id: (*id).into(),
                    kind: "kvm".into(),
                    question: "q".into(),
                    expected: "present".into(),
                    found: "present".into(),
                    raw: "1".into(),
                    confirmed: true,
                    disposition: None,
                })
                .collect(),
        }
    }

    #[test]
    fn schema_violations_catch_invalid_metadata_and_short_row_sets() {
        assert!(
            conforming_table().schema_violations().is_empty(),
            "a conforming table has no violations: {:?}",
            conforming_table().schema_violations()
        );

        let has =
            |t: &TruthTable, needle: &str| t.schema_violations().iter().any(|s| s.contains(needle));

        // An empty/whitespace `soc` or `governor` — the reviewer's examples.
        let mut t = conforming_table();
        t.identity.soc = String::new();
        assert!(has(&t, "identity.soc"));
        let mut t = conforming_table();
        t.topology.governor = "   ".into();
        assert!(has(&t, "topology.governor"));

        // core_count 0, and fewer than the 13 mandatory rows.
        let mut t = conforming_table();
        t.identity.core_count = 0;
        assert!(has(&t, "core_count"));
        let mut t = conforming_table();
        t.rows.truncate(12);
        assert!(has(&t, "minItems"));

        // A blank disposition string (must be null on a confirmed row), and a bad `kind`.
        let mut t = conforming_table();
        t.rows[0].confirmed = false;
        t.rows[0].disposition = Some(String::new());
        assert!(has(&t, "disposition"));
        let mut t = conforming_table();
        t.rows[0].kind = "bogus".into();
        assert!(has(&t, "kind"));

        // An empty required row string.
        let mut t = conforming_table();
        t.rows[0].raw = String::new();
        assert!(has(&t, "raw"));
    }

    #[test]
    fn a_confirmed_row_carries_no_disposition_a_deviation_does() {
        let rows = vec![
            RowInput::cap(
                "dev-kvm",
                "kvm",
                "q",
                Found::Present,
                Found::Present,
                "1".into(),
            ),
            RowInput::cap(
                "ecv",
                "id-register",
                "q",
                Found::Absent,
                Found::Present,
                "0xf".into(),
            ),
        ];
        let tt = assemble(identity(), topology(), rows, &BTreeMap::new());
        assert_eq!(tt.schema_version, SCHEMA_VERSION);
        assert!(tt.rows[0].confirmed);
        assert_eq!(tt.rows[0].disposition, None);
        assert!(
            !tt.rows[1].confirmed,
            "ECV found present but expected absent"
        );
        assert_eq!(tt.rows[1].disposition.as_deref(), Some(UNRULED_DEVIATION));
        assert_eq!(tt.deviations(), vec!["ecv"]);
        assert_eq!(tt.unresolved(), vec!["ecv"], "an unruled deviation gates");
        assert!(!tt.all_resolved());
    }

    #[test]
    fn a_ruled_deviation_is_acceptable_an_unruled_one_gates() {
        // A favourable, RULED deviation (ECV present, with a recorded ruling) is acceptable;
        // a second, unruled deviation still gates AA-0.
        let rows = vec![
            RowInput::cap(
                "ecv",
                "id-register",
                "q",
                Found::Absent,
                Found::Present,
                "0x1".into(),
            ),
            RowInput::cap(
                "sve",
                "id-register",
                "q",
                Found::Absent,
                Found::Present,
                "0x1".into(),
            ),
        ];
        let rulings = BTreeMap::from([(
            "ecv".to_string(),
            "FAVOURABLE: FEAT_ECV present; ruled acceptable — the paravirt clock is unaffected \
             and no stage leans on ECV (ruling 2026-07-XX)"
                .to_string(),
        )]);
        let tt = assemble(identity(), topology(), rows, &rulings);
        // ecv carries the operator's ruling, not the placeholder — resolved.
        assert!(
            tt.rows[0]
                .disposition
                .as_deref()
                .unwrap()
                .starts_with("FAVOURABLE")
        );
        // sve has no ruling — unresolved, and gates.
        assert_eq!(tt.rows[1].disposition.as_deref(), Some(UNRULED_DEVIATION));
        assert_eq!(tt.unresolved(), vec!["sve"]);
        assert!(!tt.all_resolved(), "one unruled deviation remains");
    }

    #[test]
    fn an_empty_or_whitespace_ruling_does_not_resolve_a_deviation() {
        // `{"ecv": ""}` (or whitespace) is not a disposition — it violates the schema's
        // disposition minLength and records no ruling, so it must stay UNRULED and gate.
        for empty in ["", "   ", "\t\n"] {
            let rows = vec![RowInput::cap(
                "ecv",
                "id-register",
                "q",
                Found::Absent,
                Found::Present,
                "0x1".into(),
            )];
            let rulings = BTreeMap::from([("ecv".to_string(), empty.to_string())]);
            let tt = assemble(identity(), topology(), rows, &rulings);
            assert_eq!(
                tt.rows[0].disposition.as_deref(),
                Some(UNRULED_DEVIATION),
                "an empty ruling {empty:?} is not a disposition"
            );
            assert!(!tt.all_resolved());
        }
    }

    #[test]
    fn an_unprobed_row_is_a_deviation_not_a_pass() {
        let rows = vec![RowInput::cap(
            "perf-raw-0x21-pinned",
            "perf",
            "q",
            Found::Present,
            Found::Unprobed,
            "probe failed".into(),
        )];
        let tt = assemble(identity(), topology(), rows, &BTreeMap::new());
        assert!(!tt.rows[0].confirmed, "unprobed != present");
        assert!(!tt.all_resolved());
    }

    #[test]
    fn the_truth_table_round_trips_through_json() {
        let tt = assemble(
            identity(),
            topology(),
            vec![RowInput::cap(
                "lse",
                "id-register",
                "q",
                Found::Present,
                Found::Present,
                "0x2".into(),
            )],
            &BTreeMap::new(),
        );
        assert!(tt.all_resolved(), "a confirmed-only table is acceptable");
        let json = serde_json::to_string(&tt).unwrap();
        let back: TruthTable = serde_json::from_str(&json).unwrap();
        assert_eq!(tt, back);
    }
}
