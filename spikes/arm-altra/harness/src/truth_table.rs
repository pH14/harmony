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

/// Assemble the complete truth table from the probed inputs, identity and topology.
///
/// A row is `confirmed` exactly when `found == expected`; an unconfirmed (deviation) row
/// carries the standard "unruled deviation" disposition (AA-0's acceptance demands an
/// explicit ruling, which a machine cannot invent), and a confirmed row carries `null`.
#[must_use]
pub fn assemble(identity: Identity, topology: Topology, rows: Vec<RowInput>) -> TruthTable {
    let rows = rows
        .into_iter()
        .map(|r| {
            let confirmed = r.found == r.expected;
            Row {
                id: r.id.to_string(),
                kind: r.kind.to_string(),
                question: r.question.to_string(),
                expected: r.expected,
                found: r.found,
                raw: r.raw,
                confirmed,
                disposition: if confirmed {
                    None
                } else {
                    Some(
                        "unruled deviation (found != expected): AA-0 acceptance requires an \
                         explicit ruling before any stage relies on this row"
                            .to_string(),
                    )
                },
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
    /// Whether every row is confirmed (no deviation). AA-0's RC rests on this: a deviation —
    /// even a favourable one — is a finding that needs a recorded ruling, not a pass.
    #[must_use]
    pub fn all_confirmed(&self) -> bool {
        self.rows.iter().all(|r| r.confirmed)
    }

    /// The ids of the rows that are deviations, for the operator to rule on.
    #[must_use]
    pub fn deviations(&self) -> Vec<&str> {
        self.rows
            .iter()
            .filter(|r| !r.confirmed)
            .map(|r| r.id.as_str())
            .collect()
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
        let tt = assemble(identity(), topology(), rows);
        assert_eq!(tt.schema_version, SCHEMA_VERSION);
        assert!(tt.rows[0].confirmed);
        assert_eq!(tt.rows[0].disposition, None);
        assert!(
            !tt.rows[1].confirmed,
            "ECV found present but expected absent"
        );
        assert!(tt.rows[1].disposition.is_some());
        assert!(!tt.all_confirmed());
        assert_eq!(tt.deviations(), vec!["ecv"]);
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
        let tt = assemble(identity(), topology(), rows);
        assert!(!tt.rows[0].confirmed, "unprobed != present");
        assert!(!tt.all_confirmed());
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
        );
        let json = serde_json::to_string(&tt).unwrap();
        let back: TruthTable = serde_json::from_str(&json).unwrap();
        assert_eq!(tt, back);
    }
}
