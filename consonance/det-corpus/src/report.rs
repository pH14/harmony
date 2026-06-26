// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-item report and the generic oracle runner that the binary (and, at
//! integration, the VMM registry) drive.

use crate::manifest::CorpusItem;
use crate::oracle::{
    Oracle, OracleResult, check_conformance, check_determinism, check_seed_sensitivity,
};
use serde::Serialize;
use unison::{MachineError, MachineFactory};

/// Per-item outcome: the result of every oracle the item declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ItemReport {
    /// The corpus item's name.
    pub name: String,
    /// One [`OracleResult`] per oracle in the item, in declared order.
    pub results: Vec<OracleResult>,
}

impl ItemReport {
    /// True iff the item ran **at least one** oracle and every one passed. An
    /// item with no results proved nothing, so it is never green (this guards the
    /// aggregate against a registered item that runs zero oracles).
    pub fn passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|r| r.passed)
    }
}

/// Knobs shared by every oracle in a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunConfig {
    /// Primary seed (O1/O2, and `seed_a` for O3).
    pub seed: u64,
    /// Second, distinct seed for O3 (`seed_b`).
    pub seed_b: u64,
    /// O1 checkpoint cadence in work units (must be ≥ 1).
    pub checkpoint_every: u64,
    /// Upper bound on work units to run.
    pub limit: u64,
}

/// Run every oracle declared by `item` against `factory`, returning the
/// per-item report. `read_golden` resolves a golden reference (the manifest
/// path) to its hex contents; a `Conformance` oracle whose golden does not
/// resolve is a `Fail`, not an error.
///
/// Generic over the factory: this is the "caller-supplied registry" seam — the
/// binary passes a toy factory today, the VMM registry passes `Vmm<B>` at
/// integration, with no change here.
pub fn run_item<F, G>(
    item: &CorpusItem,
    factory: &F,
    cfg: &RunConfig,
    read_golden: G,
) -> Result<ItemReport, MachineError>
where
    F: MachineFactory,
    G: Fn(&str) -> Option<String>,
{
    let mut results = Vec::with_capacity(item.oracles.len());
    for &oracle in &item.oracles {
        let result = match oracle {
            Oracle::Determinism => {
                check_determinism(factory, cfg.seed, cfg.checkpoint_every, cfg.limit)?
            }
            Oracle::Conformance => match item.golden.as_deref().and_then(&read_golden) {
                Some(hex) => check_conformance(factory, cfg.seed, cfg.limit, &hex)?,
                None => OracleResult {
                    oracle,
                    passed: false,
                    divergence: None,
                    detail: "conformance oracle has no resolvable golden".to_string(),
                },
            },
            Oracle::SeedSensitivity { rng_consuming } => {
                check_seed_sensitivity(factory, cfg.seed, cfg.seed_b, cfg.limit, rng_consuming)?
            }
        };
        results.push(result);
    }
    Ok(ItemReport {
        name: item.name.clone(),
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::CorpusKind;
    use unison::Machine;
    use unison::toy::{ToyFactory, asm, generate_program};

    fn cfg() -> RunConfig {
        RunConfig {
            seed: 7,
            seed_b: 8,
            checkpoint_every: 64,
            limit: 100_000,
        }
    }

    fn pure_factory() -> ToyFactory {
        ToyFactory {
            program: vec![asm::loadi(0, 0x1234), asm::out(0), asm::halt()],
        }
    }

    #[test]
    fn run_item_runs_each_declared_oracle_in_order() {
        // Compute the real golden for the pure program at the run seed (O2 pins the
        // observable_digest — see `check_conformance`).
        let mut m = pure_factory().spawn(cfg().seed);
        m.run_to(cfg().limit).unwrap();
        let golden = {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let mut s = String::new();
            for b in m.observable_digest() {
                s.push(char::from(HEX[usize::from(b >> 4)]));
                s.push(char::from(HEX[usize::from(b & 0x0f)]));
            }
            s
        };
        let item = CorpusItem {
            name: "pure".to_string(),
            kind: CorpusKind::Micro,
            source: "0".to_string(),
            oracles: vec![
                Oracle::Determinism,
                Oracle::Conformance,
                Oracle::SeedSensitivity {
                    rng_consuming: false,
                },
            ],
            golden: Some("golden-ref".to_string()),
        };
        let report = run_item(&item, &pure_factory(), &cfg(), |r| {
            (r == "golden-ref").then(|| golden.clone())
        })
        .unwrap();
        assert_eq!(report.results.len(), 3);
        assert_eq!(report.results[0].oracle, Oracle::Determinism);
        assert_eq!(report.results[1].oracle, Oracle::Conformance);
        assert_eq!(
            report.results[2].oracle,
            Oracle::SeedSensitivity {
                rng_consuming: false
            }
        );
        assert!(report.passed(), "{report:?}");
    }

    #[test]
    fn unresolvable_golden_is_a_fail_not_an_error() {
        let item = CorpusItem {
            name: "c".to_string(),
            kind: CorpusKind::Micro,
            source: "0".to_string(),
            oracles: vec![Oracle::Conformance],
            golden: Some("missing".to_string()),
        };
        let report = run_item(&item, &pure_factory(), &cfg(), |_| None).unwrap();
        assert!(!report.passed());
        assert!(report.results[0].detail.contains("no resolvable golden"));
    }

    #[test]
    fn determinism_passes_on_a_generated_program() {
        let f = ToyFactory {
            program: generate_program(3, 500).instrs,
        };
        let item = CorpusItem {
            name: "g".to_string(),
            kind: CorpusKind::Micro,
            source: "3".to_string(),
            oracles: vec![Oracle::Determinism],
            golden: None,
        };
        let report = run_item(&item, &f, &cfg(), |_| None).unwrap();
        assert!(report.passed());
    }

    #[test]
    fn item_with_no_oracles_is_never_green() {
        // An item that ran zero oracles proved nothing: the aggregate must not
        // count it as passed (would otherwise be a vacuous `all([]) == true`).
        let item = CorpusItem {
            name: "empty".to_string(),
            kind: CorpusKind::Micro,
            source: "0".to_string(),
            oracles: vec![],
            golden: None,
        };
        let report = run_item(&item, &pure_factory(), &cfg(), |_| None).unwrap();
        assert!(report.results.is_empty());
        assert!(
            !report.passed(),
            "an item with no oracle results is not green"
        );
    }
}
