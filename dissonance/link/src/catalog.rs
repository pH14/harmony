// SPDX-License-Identifier: AGPL-3.0-or-later
//! The link-tier **catalog** and **never-fired report**.
//!
//! **The declared set IS the catalog** (task 66 semantics). A [`Catalog`] holds
//! every point the guest SDK declared at init — name, kind, and the runtime
//! `(namespace, local-id)` coordinate its firings arrive under. [`report`] folds
//! the declared set against a run's (or campaign's) fired set into
//! `fired ⊎ never_fired`, tier-blind: a declared `sometimes` that never hit is
//! the never-fired detection, exactly as a config-declared scrape signal that
//! never matched is task 66's.
//!
//! The report shape ([`CatalogReport`]: two disjoint `BTreeSet<String>` keyed by
//! name) mirrors task 66's `matcher::Catalog`/`CatalogReport` so the two tiers
//! produce one report. Because `link` may not depend on `matcher` (surface-list
//! rule), this is the **minimal shared type**; the integrator can unify by
//! feeding these names into `matcher::Catalog::declare` (noted for task 66).

use std::collections::{BTreeMap, BTreeSet};

use explorer::{GuestEvent, Moment};
use serde::{Deserialize, Serialize};

use crate::decode::{KIND_ASSERT_HIT, KIND_ASSERT_VIOLATION, KIND_BUGGIFY, KIND_STATE, attr_u64};
use crate::read::Reader;
use crate::wire;

/// The declared kind of a catalog point — its role. Mirrors the guest SDK's
/// `PointKind`; the report can be sliced by kind (e.g. "which *sometimes* points
/// never fired"). An unrecognized kind byte decodes to [`PointKind::Unknown`]
/// (decode stays total).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PointKind {
    /// An `assert_always` point (a violation is a bug; never-firing means it held).
    AssertAlways,
    /// An `assert_sometimes` point (never-firing is a coverage gap).
    AssertSometimes,
    /// An `assert_reachable` point (never-firing means never reached).
    AssertReachable,
    /// An `assert_unreachable` point (firing is a bug).
    AssertUnreachable,
    /// An IJON state register.
    StateReg,
    /// A buggify site.
    Buggify,
    /// An unrecognized kind byte.
    Unknown,
}

impl PointKind {
    fn from_byte(b: u8) -> PointKind {
        match b {
            wire::KIND_ALWAYS => PointKind::AssertAlways,
            wire::KIND_SOMETIMES => PointKind::AssertSometimes,
            wire::KIND_REACHABLE => PointKind::AssertReachable,
            wire::KIND_UNREACHABLE => PointKind::AssertUnreachable,
            wire::KIND_STATE => PointKind::StateReg,
            wire::KIND_BUGGIFY => PointKind::Buggify,
            _ => PointKind::Unknown,
        }
    }

    /// The runtime event-id namespace a firing of this kind arrives under.
    fn namespace(self) -> u8 {
        match self {
            PointKind::StateReg => wire::NS_STATE,
            PointKind::Buggify => wire::NS_BUGGIFY,
            // All four assertion kinds fire under the assert namespace.
            _ => wire::NS_ASSERT,
        }
    }
}

/// The partition of the declared set against a fired set: `fired ⊎ never_fired =
/// declared`, always, and the two are disjoint. Keyed by name so it unifies with
/// task 66's report. Round-trips through serde (gate 2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogReport {
    /// Declared names that fired (declared ∩ fired).
    pub fired: BTreeSet<String>,
    /// Declared names that never fired (declared − fired) — the detection.
    pub never_fired: BTreeSet<String>,
}

/// The declared signal set of one run/campaign, parsed from the SDK catalog
/// declaration. Deterministically ordered (`BTreeMap`s), so no iteration order
/// reaches a report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    /// Declared name → kind.
    declared: BTreeMap<String, PointKind>,
    /// Runtime coordinate `(namespace, local id)` → declared name, so a firing
    /// event maps back to the point it names.
    by_coord: BTreeMap<(u8, u32), String>,
    /// Declared name → its current coordinate, so a re-declare can drop the
    /// name's **stale** coordinate from `by_coord` (else a firing at the old
    /// coordinate would still resolve to the name).
    coord_of_name: BTreeMap<String, (u8, u32)>,
}

impl Catalog {
    /// An empty catalog.
    pub fn new() -> Catalog {
        Catalog::default()
    }

    /// Parse the SDK catalog-declaration blob (the payload of the `event_id == 0`
    /// Emit). **Total**: malformed or truncated input yields whatever prefix
    /// parsed cleanly, never a panic — a hostile guest cannot crash the fold. A
    /// point whose kind byte is unrecognized is kept with [`PointKind::Unknown`].
    pub fn from_declaration_bytes(bytes: &[u8]) -> Catalog {
        let mut cat = Catalog::new();
        let mut r = Reader::new(bytes);
        // Header: magic + version + count. A bad magic/version yields an empty
        // catalog rather than a misparse.
        if r.u32() != Some(wire::CATALOG_MAGIC) || r.u8() != Some(wire::SDK_WIRE_VERSION) {
            return cat;
        }
        let Some(count) = r.u32() else {
            return cat;
        };
        for _ in 0..count {
            let (Some(kind_byte), Some(local), Some(name_bytes)) =
                (r.u8(), r.u32(), r.bytes_lp16())
            else {
                break; // truncated — keep what parsed
            };
            let name = String::from_utf8_lossy(name_bytes).into_owned();
            let kind = PointKind::from_byte(kind_byte);
            cat.declare(name, local, kind);
        }
        cat
    }

    /// Find the SDK catalog declaration in a raw captured event stream (the
    /// `event_id == 0` Emit) and parse it. Total; an absent declaration yields an
    /// empty catalog.
    pub fn from_raw_stream(raw: &[(Moment, u32, Vec<u8>)]) -> Catalog {
        match raw.iter().find(|(_, id, _)| *id == wire::CATALOG_EVENT_ID) {
            Some((_, _, bytes)) => Catalog::from_declaration_bytes(bytes),
            None => Catalog::new(),
        }
    }

    /// Register one declared point. A re-declared name updates its kind and
    /// coordinate (idempotent on the name), mirroring task 66's `declare` — and
    /// **removes the name's previous coordinate** from `by_coord` when it moves,
    /// so no stale coordinate survives to resolve a firing to this name.
    fn declare(&mut self, name: String, local: u32, kind: PointKind) {
        let coord = (kind.namespace(), local);
        if let Some(&old) = self.coord_of_name.get(&name)
            && old != coord
        {
            self.by_coord.remove(&old);
        }
        self.coord_of_name.insert(name.clone(), coord);
        self.by_coord.insert(coord, name.clone());
        self.declared.insert(name, kind);
    }

    /// The declared points with their kinds, deterministically ordered.
    pub fn declared(&self) -> impl Iterator<Item = (&String, &PointKind)> {
        self.declared.iter()
    }

    /// The number of declared points.
    pub fn len(&self) -> usize {
        self.declared.len()
    }

    /// Whether nothing was declared.
    pub fn is_empty(&self) -> bool {
        self.declared.is_empty()
    }

    /// The set of **declared** names that fired at least once in `events` (a
    /// decoded event stream, e.g. [`RunTrace::events`](explorer::RunTrace)). A
    /// firing whose coordinate was never declared is ignored — the report is over
    /// the declared set.
    pub fn fired(&self, events: &[(Moment, GuestEvent)]) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for (_, ev) in events {
            if let Some(coord) = firing_coord(ev)
                && let Some(name) = self.by_coord.get(&coord)
            {
                out.insert(name.clone());
            }
        }
        out
    }

    /// Partition the declared set against `fired` into `fired ⊎ never_fired`. Ids
    /// in `fired` that were never declared are ignored, so the union is exactly
    /// the declared set by construction (tier-blind).
    pub fn report(&self, fired: &BTreeSet<String>) -> CatalogReport {
        let mut report = CatalogReport::default();
        for name in self.declared.keys() {
            if fired.contains(name) {
                report.fired.insert(name.clone());
            } else {
                report.never_fired.insert(name.clone());
            }
        }
        report
    }

    /// The end-to-end fold: parse the declaration, compute the fired set from the
    /// decoded stream, and report. The one call a campaign makes per run.
    pub fn fold(declaration: &[u8], events: &[(Moment, GuestEvent)]) -> CatalogReport {
        let cat = Catalog::from_declaration_bytes(declaration);
        let fired = cat.fired(events);
        cat.report(&fired)
    }
}

/// The `(namespace, local id)` a decoded **firing** event names, or `None` if the
/// event is not a firing (the catalog declaration, `setup_complete`, or an
/// unknown event never mark a declared point fired).
fn firing_coord(ev: &GuestEvent) -> Option<(u8, u32)> {
    let (ns, key) = match ev.kind.as_str() {
        KIND_ASSERT_HIT | KIND_ASSERT_VIOLATION => (wire::NS_ASSERT, "point"),
        KIND_STATE => (wire::NS_STATE, "reg"),
        KIND_BUGGIFY => (wire::NS_BUGGIFY, "point"),
        _ => return None,
    };
    let local = attr_u64(ev, key)?;
    // Local ids are 24-bit; a decoded value always fits, but guard defensively.
    if local > wire::LOCAL_MASK as u64 {
        return None;
    }
    Some((ns, local as u32))
}
