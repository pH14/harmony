//! The ARM spike evidence floor checker.
//!
//! Given a retained run-set — a `run-set.json` manifest plus a `records.jsonl`
//! file, the two shapes defined in [`arm_harness::evidence`] — this crate
//! **recomputes every acceptance floor from the raw per-sample records** and
//! never trusts a summary. That is the whole reason it exists: `docs/ARM-ALTRA.md`
//! §Evidence integrity was written after the PR-98 review found harnesses that
//! reported green on failed gates, dispositions whose floors the retained evidence
//! did not meet, and an existential-stage harness that silently exercised the stock
//! fallback while claiming the patched mechanism. The manifest deliberately carries
//! **no** result totals to believe; the only numbers this checker uses are the ones
//! it derives from the records, whose sha256 the manifest pins.
//!
//! # The six countermeasures, made mechanical
//!
//! Each §Evidence-integrity rule becomes a check here:
//!
//! 1. **Gate-RC propagation** — the checker's exit status is the conjunction of
//!    every check; a "reached the end" condition is never success.
//! 2. **Machine-checked floors** — counts, rep floors and armed-overflow floors are
//!    recomputed from the records, not read from the manifest.
//! 3. **Content-hash-verified boots** — every [`ImagePin`](arm_harness::evidence::ImagePin)
//!    must carry `verified_before_boot == true`.
//! 4. **Mechanism attestation** — every record's exit reason must equal the
//!    manifest's claimed [`Mechanism::expected_exit_reason`](arm_harness::evidence::Mechanism);
//!    a patched claim demands the marker was observed.
//! 5. **Independent oracle** — counts are judged against
//!    [`oracle_model::expected`], never PMU-vs-PMU.
//! 6. **Multiplicity + totality** — exactly-once is shown per record, and every
//!    attempted sample must appear.
//!
//! # No invented constants
//!
//! [`Weights`](oracle_model::Weights) and the skid margin are stage deliverables,
//! not defaults. When the manifest carries `None` for either, the checker
//! **refuses** the affected check and exits nonzero — it never substitutes a guess.
//!
//! # Untested on silicon
//!
//! Nothing here has judged real hardware evidence; the fixtures it is tested
//! against are synthesised from the oracle model. The checker is nonetheless the
//! arrival-day authority: a stage disposition may rest on its verdict, never on a
//! harness's own done-marker.

pub mod check;
pub mod error;
pub mod fixtures;

pub use check::{CheckId, CheckReport, Floors, Outcome, Status, check_run_set};
pub use error::LoadError;
