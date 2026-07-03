// SPDX-License-Identifier: AGPL-3.0-or-later
//! # logtmpl — the log-template scrape sensor + CellFn v1
//!
//! The scrape tier is the primary signal channel (`docs/EXPLORATION.md`): a
//! running system tells you what state it is in *on its console*. But console
//! logs are open-vocabulary — raw text cannot be a [`FeatureId`]. This crate is
//! the standard fix, [Drain]-style **log-template clustering**: strip the
//! parameters, cluster lines into template *species*, and the low-cardinality
//! species stream becomes a stable signal. It ships three things behind the
//! search-plane spine (task 64):
//!
//! - a [`LogSensor`] (spine [`Sensor`](explorer::Sensor)) that turns a run's log
//!   records into a timestamped template-species [`Feature`](explorer::Feature)
//!   stream, stabilized by a codebook **internal to this crate**;
//! - a [`TemplateRecord`] (spine [`Matchable`](explorer::Matchable)) that adapts
//!   a log line + its template to the matcher DSL (task 66) — `kind`/`msg`/
//!   `template`/`param.N`/`moment` — with no dependency between the two crates;
//! - [`CellFnV1`] (spine [`CellFn`](explorer::CellFn)), the first multi-channel,
//!   point-in-time, **bounded** cell function.
//!
//! ## Codebook internality (the EXPLORATION ruling)
//!
//! Stable [`FeatureId`]s cross this crate's boundary; template text, tree
//! structure, and clustering thresholds never do. The spine and the explorer
//! never learn that clustering exists. The codebook (internal — `pub(crate)`) is
//! a deterministic, all-integer fold whose byte-identical serialization survives
//! serialize → reload → continue (gates 2–3), persisted only as the opaque bytes
//! of [`LogSensor::codebook_bytes`]/[`LogSensor::with_codebook_bytes`].
//!
//! ## Determinism discipline
//!
//! Every container is a `BTreeMap`/`BTreeSet`/`Vec`; there is no
//! `HashMap`/`HashSet`, no floating point (the similarity threshold is an
//! integer cross-multiply), and no wall-clock or entropy anywhere — so the same
//! log stream always yields the same species set, the same ids, and the same
//! bytes. Library code never panics on untrusted input.
//!
//! [`FeatureId`]: explorer::FeatureId
//! [Drain]: https://pinjiadb.github.io/publication/icws17-drain/

mod cell;
mod cluster;
mod error;
mod loader;
mod record;
mod sensor;
mod token;

pub use cell::{
    CellConfig, CellFnV1, DEFAULT_FOLD_K, Quant, decode_cell_key, encode_cell_key, log2_bucket,
};
pub use error::{Error, Result};
pub use loader::load_console_log;
pub use record::TemplateRecord;
pub use sensor::{LogSensor, TEMPLATE_CHANNEL};

// The codebook, its config, and the template-token vocabulary are **internal**
// (the EXPLORATION internality ruling: "nothing codebook-shaped appears in any
// public signature that the spine or another crate could couple to"). They are
// `pub(crate)` in `cluster`/`token` and deliberately not re-exported; a campaign
// persists the fold through the opaque bytes of
// [`LogSensor::codebook_bytes`]/[`LogSensor::with_codebook_bytes`], never a
// codebook-shaped type.
