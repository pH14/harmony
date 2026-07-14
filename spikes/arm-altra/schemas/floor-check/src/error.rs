// SPDX-License-Identifier: AGPL-3.0-or-later
//! Load errors.
//!
//! These are the failures that stop the checker before it can even judge the
//! evidence — a manifest that will not read or parse, a records file that is not
//! valid JSONL. They are distinct from a *check* failure (a run-set that loaded
//! fine but did not meet a floor): a load error means the evidence itself is
//! unreadable, and the checker exits nonzero with the reason rather than guessing
//! at what the bytes meant.

use std::path::PathBuf;

/// Why a run-set could not be loaded for checking.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// The manifest file could not be read.
    #[error("cannot read manifest {path}: {source}")]
    ReadManifest {
        /// The manifest path.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// The manifest was not valid JSON for a [`RunSet`](arm_harness::evidence::RunSet).
    #[error("cannot parse manifest {path}: {source}")]
    ParseManifest {
        /// The manifest path.
        path: PathBuf,
        /// The underlying serde error.
        source: serde_json::Error,
    },

    /// The records file could not be read.
    #[error("cannot read records {path}: {source}")]
    ReadRecords {
        /// The records path.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// A line of the records file was not valid JSON for a
    /// [`RunRecord`](arm_harness::evidence::RunRecord). A malformed record is
    /// unreadable evidence, not a check to grade — the line number is 1-based.
    #[error("cannot parse record on line {line} of {path}: {source}")]
    ParseRecord {
        /// The records path.
        path: PathBuf,
        /// The 1-based line number.
        line: usize,
        /// The underlying serde error.
        source: serde_json::Error,
    },
}
