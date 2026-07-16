// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arm64 host-homogeneity probe (`docs/ARCH-BOUNDARY.md` §B, ARM row) — the
//! arm64 analogue of x86's `hostassert`: `MIDR` / `ID_AA64*` / errata behind the
//! same [`enforce`] gate the composition root runs before the first boot.
//!
//! **A skeleton probe.** The concrete `expect-vs-found` rows (which `ID_AA64*`
//! fields must match the frozen contract, which errata must be present) are
//! AA-0's capability truth table + AA-6's enforcement rows (`docs/ARM-ALTRA.md`)
//! — measured on real N1 silicon, never guessed here. The live probe is
//! `target_arch = "aarch64"`-gated and reads nothing yet (its row set is
//! `TODO(AA-0/AA-6)`); off the box it is a no-op, exactly as x86's is off an
//! x86 box. It exists so the boot root has the same `enforce()` seam both
//! vendors share.

use crate::vmm::VmmError;

/// One host-assertion outcome (the arm64 twin of x86's `Outcome`): an
/// `expect-vs-found` row and whether it passed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outcome {
    /// What was checked (e.g. an `ID_AA64*` field name).
    pub key: String,
    /// The frozen/expected value.
    pub expected: String,
    /// The value found on the host.
    pub actual: String,
    /// Whether the row passed.
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

/// The host-homogeneity report. Off the arm64 box (or under Miri) a single
/// skipped-but-passing row; the real `ID_AA64*`/`MIDR`/errata probe is
/// arrival-day (`TODO(AA-0/AA-6)`).
pub fn report() -> Vec<Outcome> {
    // The concrete probe rows are the spike's measured truth table. Until then
    // the seam reports one honest "skipped" row on every host — the arm64
    // mirror of x86's off-box skip.
    vec![Outcome::new(
        "arm64-host-assert",
        "AA-0/AA-6 truth table",
        "skipped (skeleton: no rows ruled yet)",
        true,
    )]
}

/// Enforce the host baseline before the first boot: `Ok` iff every row passes,
/// else [`VmmError::HostAssert`] naming the failures. Split from [`report`] so
/// the all-pass branch is testable on every platform.
pub(crate) fn enforce() -> Result<(), VmmError> {
    verdict(report())
}

fn verdict(outcomes: Vec<Outcome>) -> Result<(), VmmError> {
    let failures: Vec<String> = outcomes
        .iter()
        .filter(|o| !o.pass)
        .map(|o| format!("{}: expected {}, found {}", o.key, o.expected, o.actual))
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

    #[test]
    fn report_is_well_formed_and_enforce_agrees() {
        let r = report();
        assert!(!r.is_empty());
        assert!(enforce().is_ok());
    }

    #[test]
    fn verdict_is_ok_iff_all_outcomes_pass() {
        assert!(verdict(vec![Outcome::new("a", "x", "x", true)]).is_ok());
        let err = verdict(vec![
            Outcome::new("a", "x", "x", true),
            Outcome::new("b", "y", "z", false),
        ])
        .unwrap_err();
        assert!(matches!(err, VmmError::HostAssert(_)));
    }
}
