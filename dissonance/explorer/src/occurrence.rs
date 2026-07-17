// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **occurrence-counterexample Oracle** and the finalized **absence
//! expectations** view (`hm-bbx.4`).
//!
//! Two distinct assertion-judgment mechanisms the strategy keeps separate:
//!
//! - [`OccurrenceOracle`] — a *pure per-run* judgment over one borrowed,
//!   immutable [`CompletedRunEvidence`] view (supplied only **after** durable
//!   append). It reports occurrence counterexamples — an `always` evaluating
//!   false, an `unreachable` point firing, or a binary-terminal
//!   [`StopReason::Assertion`] — and **deduplicates by property** (the aggregated
//!   property identity, never the site). Site identity is preserved as
//!   provenance/coverage but is not the verdict key.
//! - [`AbsenceLedger`] — a *finalized, cross-run* view over explicit `must_hit`
//!   property expectations minus aggregated property results. A `sometimes`/
//!   `reachable` property that no run satisfied is an absence finding. Its counts
//!   are **retention-stable** (monotone, finalized): expiring working-set
//!   membership or GC-ing raw evidence can never resurrect a false never-fired
//!   finding, and an unresolved-reducer state declaration is reportable coverage,
//!   never an automatically-failed expectation.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::evidence::CompletedRunEvidence;
use crate::spine::Moment;
use sdk_events::{AssertType, Expectation, ObservationId, Payload, SdkEvent, SiteId};

use crate::sdk_moment_to_spine;
use sdk_events::NS_ASSERT;

/// What kind of occurrence counterexample was observed. All three are judged over
/// the immutable completed-run view; none is a state or absence claim.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum CounterexampleKind {
    /// An `always` assertion evaluated false (a JSON or binary firing with a
    /// false condition).
    AlwaysViolated,
    /// An `unreachable` point fired — reaching it is the violation.
    UnreachableReached,
    /// The rollout terminated on a binary-plane [`StopReason::Assertion`] the
    /// vmm-core run-loop surfaced (the role the retired `AlwaysViolation` oracle
    /// played, now judged over the evidence view).
    TerminalAssertion,
}

/// One occurrence counterexample, keyed by the **property** it violates. Multiple
/// sites or repeated evaluations of one property collapse to a single
/// counterexample (dedup by property); the `site` is preserved as provenance and
/// coverage, deliberately **not** part of the dedup identity.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OccurrenceCounterexample {
    /// The aggregated property identity the counterexample belongs to (the dedup
    /// key).
    pub property: ObservationId,
    /// Which occurrence rule the evidence violated.
    pub kind: CounterexampleKind,
    /// The V-time the counterexample was first observed at (for localization).
    pub at: Moment,
    /// The assertion site — provenance/coverage only, never the verdict key.
    pub site: Option<SiteId>,
    /// A stable digest over `(property, kind)` so equivalent verdicts dedup
    /// across the many runs and sites that reach the same property.
    pub fingerprint: [u8; 32],
}

/// The occurrence-counterexample Oracle: a pure judgment over one completed-run
/// evidence view. Stateless.
#[derive(Clone, Debug, Default)]
pub struct OccurrenceOracle;

impl OccurrenceOracle {
    /// The occurrence oracle (stateless).
    pub fn new() -> Self {
        Self
    }

    /// Judge one completed run's immutable evidence, returning its occurrence
    /// counterexamples **deduplicated by property** (first observation wins for
    /// the reported `at`/`site`), in canonical property order. Both the JSON
    /// assertion events and a binary-terminal [`StopReason::Assertion`] use this
    /// same view.
    pub fn judge(&self, ev: &CompletedRunEvidence) -> Vec<OccurrenceCounterexample> {
        // Dedup by (property, kind): a property that violates once is one
        // counterexample however many sites/evaluations reached it. BTreeMap
        // keeps the output canonically ordered (determinism).
        let mut found: BTreeMap<(ObservationId, CounterexampleKind), OccurrenceCounterexample> =
            BTreeMap::new();

        // JSON / binary assertion events on the data plane.
        for sev in &ev.normalized.events {
            let Payload::Assertion {
                assert_type,
                condition,
            } = &sev.payload
            else {
                continue;
            };
            let kind = match assert_type {
                Some(AssertType::Always) if *condition == Some(false) => {
                    CounterexampleKind::AlwaysViolated
                }
                // An `unreachable` firing at all is the violation (the decoder
                // only mints an unreachable event on the violation disposition).
                Some(AssertType::Unreachable) => CounterexampleKind::UnreachableReached,
                _ => continue,
            };
            insert_counterexample(&mut found, sev.id.clone(), kind, sev, ev);
        }

        // The binary-plane terminal assertion the run-loop surfaced.
        if let crate::StopReason::Assertion { vtime, id, .. } = &ev.terminal {
            let property = ObservationId::Point {
                namespace: NS_ASSERT,
                local: id & 0x00FF_FFFF,
            };
            found
                .entry((property.clone(), CounterexampleKind::TerminalAssertion))
                .or_insert_with(|| OccurrenceCounterexample {
                    fingerprint: fingerprint(&property, CounterexampleKind::TerminalAssertion),
                    property,
                    kind: CounterexampleKind::TerminalAssertion,
                    at: *vtime,
                    site: None,
                });
        }

        found.into_values().collect()
    }
}

/// Record a data-plane counterexample under its property, first-observation-wins.
fn insert_counterexample(
    found: &mut BTreeMap<(ObservationId, CounterexampleKind), OccurrenceCounterexample>,
    property: ObservationId,
    kind: CounterexampleKind,
    sev: &SdkEvent,
    _ev: &CompletedRunEvidence,
) {
    found
        .entry((property.clone(), kind))
        .or_insert_with(|| OccurrenceCounterexample {
            fingerprint: fingerprint(&property, kind),
            property,
            kind,
            at: sdk_moment_to_spine(sev.moment),
            site: sev.site.clone(),
        });
}

/// A stable 32-byte digest over `(property, kind)` — domain-separated so an
/// occurrence counterexample dedups across every site and run that reaches the
/// same property, and can never collide with a different property or kind.
fn fingerprint(property: &ObservationId, kind: CounterexampleKind) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"dissonance.explorer.occurrence.v1");
    h.update([kind as u8]);
    match property {
        ObservationId::Point { namespace, local } => {
            h.update([0x01, *namespace]);
            h.update(local.to_le_bytes());
        }
        ObservationId::Property(s) => {
            h.update([0x02]);
            h.update((s.len() as u64).to_le_bytes());
            h.update(s.as_bytes());
        }
        ObservationId::Lifecycle(s) => {
            h.update([0x03]);
            h.update((s.len() as u64).to_le_bytes());
            h.update(s.as_bytes());
        }
    }
    h.finalize().into()
}

// ---------------------------------------------------------------------------
// Finalized absence expectations
// ---------------------------------------------------------------------------

/// One finalized property aggregate: its declared `must_hit` expectation and a
/// **monotone** satisfied count. The count only ever rises, so raw-evidence GC or
/// working-set expiration can never move it backward and resurrect a false
/// never-fired finding.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
struct PropertyAggregate {
    expectation: Option<Expectation>,
    /// How many satisfying evaluations were observed across all folded runs
    /// (finalized, monotone).
    satisfied: u64,
    /// The declared human name, for reporting.
    name: Option<String>,
}

/// One absence finding: a `must_hit` property (a `sometimes`/`reachable`, or an
/// Antithesis `must_hit`) that **no** completed run satisfied.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AbsenceFinding {
    /// The property that was expected to be hit but never was.
    pub property: ObservationId,
    /// The declared expectation (always [`Expectation::MustHit`] here).
    pub expectation: Expectation,
    /// The declared human name, if any.
    pub name: Option<String>,
}

/// The finalized absence-expectations view: a cross-run fold over explicit
/// `must_hit` property expectations minus aggregated property results.
///
/// This is **not** a per-trace oracle and **not** a per-site subtraction — it is
/// a finalized reduction over property aggregates. Fold each completed run's
/// evidence in with [`observe`](Self::observe); read the surviving never-satisfied
/// `must_hit` properties with [`absences`](Self::absences). Counts are monotone
/// and retention-stable by construction. An unresolved-reducer **state**
/// declaration is never tracked here (it is site/schema coverage, not an
/// expectation), so it can never become a spurious absence.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct AbsenceLedger {
    properties: BTreeMap<ObservationId, PropertyAggregate>,
}

impl AbsenceLedger {
    /// An empty absence ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one completed run's immutable evidence into the finalized aggregates:
    /// register every declared `must_hit` property, and increment the satisfied
    /// count for every property a satisfying evaluation was observed for. Only
    /// **assertion** expectations are tracked; a state declaration with an
    /// unresolved reducer is coverage, not an expectation, and is skipped.
    pub fn observe(&mut self, ev: &CompletedRunEvidence) {
        // Register declared must-hit expectations from the schema.
        for entry in ev.normalized.schema.entries() {
            if entry.expectation == Some(Expectation::MustHit) {
                let agg = self.properties.entry(entry.id.clone()).or_default();
                agg.expectation = Some(Expectation::MustHit);
                if agg.name.is_none() {
                    agg.name = entry.name.clone();
                }
            }
        }
        // Count satisfying evaluations: a `sometimes`/`reachable` hit (condition
        // true, or a reachable firing) satisfies its must-hit property.
        for sev in &ev.normalized.events {
            if let Payload::Assertion {
                assert_type,
                condition,
            } = &sev.payload
                && satisfies_must_hit(assert_type, condition)
            {
                let agg = self.properties.entry(sev.id.clone()).or_default();
                // A firing implies the property is declared must-hit-capable even
                // if the schema entry was pending; record it so `absences` is a
                // clean set-difference. `saturating_add` keeps the count monotone.
                if agg.expectation.is_none() {
                    agg.expectation = Some(Expectation::MustHit);
                }
                agg.satisfied = agg.satisfied.saturating_add(1);
            }
        }
    }

    /// The finalized absence findings: every tracked `must_hit` property with a
    /// zero satisfied count, in canonical property order.
    pub fn absences(&self) -> Vec<AbsenceFinding> {
        self.properties
            .iter()
            .filter(|(_, agg)| agg.expectation == Some(Expectation::MustHit) && agg.satisfied == 0)
            .map(|(property, agg)| AbsenceFinding {
                property: property.clone(),
                expectation: Expectation::MustHit,
                name: agg.name.clone(),
            })
            .collect()
    }

    /// The finalized satisfied count for a property (retention-stable), for
    /// inspection.
    pub fn satisfied(&self, property: &ObservationId) -> u64 {
        self.properties
            .get(property)
            .map(|a| a.satisfied)
            .unwrap_or(0)
    }
}

/// Whether an assertion evaluation **satisfies** a `must_hit` expectation: a
/// `sometimes` holding true, or a `reachable` point being reached. `always`/
/// `unreachable` are not must-hit satisfiers (their absence semantics differ).
fn satisfies_must_hit(assert_type: &Option<AssertType>, condition: &Option<bool>) -> bool {
    match assert_type {
        Some(AssertType::Sometimes) => *condition == Some(true),
        Some(AssertType::Reachable) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::RunId;
    use crate::spine::EvidenceCut;
    use crate::{Reproducer, StopReason};
    use sdk_events::{Normalized, decode_antithesis, decode_binary};

    fn env() -> Reproducer {
        Reproducer {
            blob_version: 1,
            bytes: vec![],
        }
    }

    fn evidence(
        terminal: StopReason,
        normalized: Normalized,
        included: u64,
    ) -> CompletedRunEvidence {
        CompletedRunEvidence {
            rollout: RunId {
                issue: 0,
                parent: None,
            },
            terminal,
            env: env(),
            cut: EvidenceCut {
                at: Moment(100),
                sdk_events: included,
            },
            normalized,
        }
    }

    fn quiescent() -> StopReason {
        StopReason::Quiescent { vtime: Moment(100) }
    }

    fn json(records: &[&str]) -> Normalized {
        let recs: Vec<(sdk_events::Moment, Vec<u8>)> = records
            .iter()
            .enumerate()
            .map(|(i, s)| (sdk_events::Moment(i as u64), s.as_bytes().to_vec()))
            .collect();
        decode_antithesis(&recs).expect("decodes")
    }

    /// A JSON `always` evaluating false is an occurrence counterexample keyed by
    /// its property (the message), with the site preserved separately.
    #[test]
    fn json_always_false_is_a_property_counterexample() {
        let n = json(&[
            r#"{"antithesis_assert":{"assert_type":"always","condition":false,
            "id":"site-1","message":"invariant holds","must_hit":false,
            "location":{"file":"a.rs","function":"f","begin_line":1,"begin_column":1}}}"#,
        ]);
        let ev = evidence(quiescent(), n, u64::MAX);
        let ces = OccurrenceOracle::new().judge(&ev);
        assert_eq!(ces.len(), 1);
        assert_eq!(ces[0].kind, CounterexampleKind::AlwaysViolated);
        assert_eq!(
            ces[0].property,
            ObservationId::Property("invariant holds".into())
        );
        // The site is preserved as provenance, separate from the property key.
        assert_eq!(ces[0].site.as_ref().unwrap().id.as_deref(), Some("site-1"));
    }

    /// Multiple sites of one property collapse to a single counterexample (dedup
    /// by property, not by site).
    #[test]
    fn multiple_sites_of_one_property_dedup_to_one() {
        let n = json(&[
            r#"{"antithesis_assert":{"assert_type":"always","condition":false,"id":"a",
                "message":"p","location":{"file":"a.rs","function":"f","begin_line":1,"begin_column":1}}}"#,
            r#"{"antithesis_assert":{"assert_type":"always","condition":false,"id":"b",
                "message":"p","location":{"file":"b.rs","function":"g","begin_line":2,"begin_column":2}}}"#,
        ]);
        let ev = evidence(quiescent(), n, u64::MAX);
        let ces = OccurrenceOracle::new().judge(&ev);
        assert_eq!(ces.len(), 1, "two sites, one property → one counterexample");
        assert_eq!(ces[0].property, ObservationId::Property("p".into()));
    }

    /// A JSON `always` holding true is not a counterexample.
    #[test]
    fn json_always_true_is_not_a_counterexample() {
        let n = json(&[
            r#"{"antithesis_assert":{"assert_type":"always","condition":true,"message":"ok"}}"#,
        ]);
        let ev = evidence(quiescent(), n, u64::MAX);
        assert!(OccurrenceOracle::new().judge(&ev).is_empty());
    }

    /// A binary-terminal `StopReason::Assertion` is an occurrence counterexample
    /// over the evidence view (the retired `AlwaysViolation` role).
    #[test]
    fn binary_terminal_assertion_is_a_counterexample() {
        let ev = evidence(
            StopReason::Assertion {
                vtime: Moment(50),
                id: 7,
                data: vec![],
            },
            json(&[]),
            0,
        );
        let ces = OccurrenceOracle::new().judge(&ev);
        assert_eq!(ces.len(), 1);
        assert_eq!(ces[0].kind, CounterexampleKind::TerminalAssertion);
        assert_eq!(ces[0].at, Moment(50));
    }

    /// The v1 catalog wire: magic + version + count + per-point (kind, local,
    /// name). `KIND_UNREACHABLE` = 3 declares the assert verb the v2 path omits.
    fn v1_catalog(points: &[(u8, u32, &str)]) -> Vec<u8> {
        let magic = u32::from_le_bytes(*b"SDKC");
        let mut b = Vec::new();
        b.extend_from_slice(&magic.to_le_bytes());
        b.push(1);
        b.extend_from_slice(&(points.len() as u32).to_le_bytes());
        for (kind, local, name) in points {
            b.push(*kind);
            b.extend_from_slice(&local.to_le_bytes());
            b.extend_from_slice(&(name.len() as u16).to_le_bytes());
            b.extend_from_slice(name.as_bytes());
        }
        b
    }

    /// A binary `unreachable` firing is a counterexample; the same violation on
    /// two runs shares a fingerprint (dedup across runs).
    #[test]
    fn binary_unreachable_firing_is_a_counterexample() {
        const KIND_UNREACHABLE: u8 = 3;
        let decl = v1_catalog(&[(KIND_UNREACHABLE, 3, "never")]);
        // An unreachable point fires on the VIOLATION disposition (reaching it).
        let firing = {
            let mut b = vec![1u8]; // DISP_VIOLATION
            b.extend_from_slice(&0u16.to_le_bytes()); // empty detail
            b
        };
        let id = ((sdk_events::NS_ASSERT as u32) << 24) | 3;
        let n = decode_binary(&[
            (sdk_events::Moment(0), 0, decl),
            (sdk_events::Moment(5), id, firing),
        ])
        .expect("decodes");
        let ev = evidence(quiescent(), n, u64::MAX);
        let ces = OccurrenceOracle::new().judge(&ev);
        assert_eq!(ces.len(), 1);
        assert_eq!(ces[0].kind, CounterexampleKind::UnreachableReached);
        // A second identical run's counterexample shares the fingerprint.
        let ces2 = OccurrenceOracle::new().judge(&ev);
        assert_eq!(ces[0].fingerprint, ces2[0].fingerprint);
    }

    /// A `sometimes` property that no run satisfied is an absence finding;
    /// satisfying it on any run clears it, and the satisfied count is monotone.
    #[test]
    fn never_satisfied_sometimes_is_a_retention_stable_absence() {
        // Declare a `sometimes` property (must-hit) via a v1 catalog; never fire it.
        let catalog = {
            let magic = u32::from_le_bytes(*b"SDKC");
            let mut b = Vec::new();
            b.extend_from_slice(&magic.to_le_bytes());
            b.push(1); // v1
            b.extend_from_slice(&1u32.to_le_bytes()); // one point
            b.push(1); // KIND_SOMETIMES
            b.extend_from_slice(&5u32.to_le_bytes()); // local 5
            b.extend_from_slice(&(1u16).to_le_bytes());
            b.extend_from_slice(b"p");
            b
        };
        let never = decode_binary(&[(sdk_events::Moment(0), 0, catalog.clone())]).expect("decodes");
        let mut led = AbsenceLedger::new();
        led.observe(&evidence(quiescent(), never, 0));
        let absences = led.absences();
        assert_eq!(absences.len(), 1);
        let prop = ObservationId::Point {
            namespace: sdk_events::NS_ASSERT,
            local: 5,
        };
        assert_eq!(absences[0].property, prop);
        assert_eq!(led.satisfied(&prop), 0);

        // Now a run satisfies it (a HIT). The absence clears and stays cleared —
        // folding the never-firing run again cannot resurrect it (monotone count).
        let firing = {
            let mut b = vec![0u8]; // DISP_HIT
            b.extend_from_slice(&0u16.to_le_bytes());
            b
        };
        let id = ((sdk_events::NS_ASSERT as u32) << 24) | 5;
        let hit = decode_binary(&[
            (sdk_events::Moment(0), 0, catalog.clone()),
            (sdk_events::Moment(3), id, firing),
        ])
        .expect("decodes");
        led.observe(&evidence(quiescent(), hit, u64::MAX));
        assert!(
            led.absences().is_empty(),
            "a satisfying run clears the absence"
        );
        assert!(led.satisfied(&prop) >= 1);
        // Re-observing the never-firing run does not move the count backward.
        let never2 = decode_binary(&[(sdk_events::Moment(0), 0, catalog)]).expect("decodes");
        led.observe(&evidence(quiescent(), never2, 0));
        assert!(
            led.absences().is_empty(),
            "expiration/re-fold cannot resurrect a satisfied property"
        );
    }
}
