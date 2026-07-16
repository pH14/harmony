// SPDX-License-Identifier: AGPL-3.0-or-later
//! Determinism & conformance corpus — the domain layer over [`unison`].
//!
//! `unison` is a generic, domain-free divergence bisector over a
//! [`unison::Subject`]. This crate turns that primitive into the three
//! determinism oracles of `docs/DETERMINISM-CORPUS.md` — O1 determinism
//! ([`check_determinism`]), O2 conformance ([`check_conformance`]), and O3
//! seed-sensitivity ([`check_seed_sensitivity`]) — plus a [corpus
//! manifest](load_manifest) that records which oracles apply to which workload,
//! and a JSON report ([`ItemReport`]). Everything is written generically over
//! [`unison::SubjectFactory`], so it is fully testable today against the toy
//! machine and is pointed at the real `Vmm<B>` at integration with no API
//! change.
//!
//! O4 (backend-equivalence) is intentionally absent: it needs two real backends
//! and is trivially `unison::compare_runs(F_kvm, F_patched, …)` once they exist
//! — see `IMPLEMENTATION.md`.

#![warn(missing_docs)]

mod manifest;
mod oracle;
mod registry;
mod report;

pub use manifest::{CorpusItem, CorpusKind, ManifestError, load_manifest, to_manifest, validate};
pub use oracle::{
    Oracle, OracleResult, check_conformance, check_determinism, check_seed_sensitivity,
};
pub use registry::{TOY_MIN_WORK, toy_factory};
pub use report::{ItemReport, RunConfig, run_item};
