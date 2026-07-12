// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crate's error type. Every fallible path in the harness is *loud*: a
//! corpus that does not hash as the manifest says, a replay that does not
//! reproduce the recorded campaign, a candidate that does not reproduce the
//! control — each aborts rather than degrading into a plausible number.

use std::path::PathBuf;

/// The harness's result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong loading, verifying, or re-keying the corpus.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file named by the manifest could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The offending path.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// JSON that does not parse as the shape the manifest promises.
    #[error("cannot parse {what}: {source}")]
    Json {
        /// What was being parsed (a path, or a member name inside an archive).
        what: String,
        /// The underlying serde failure.
        source: serde_json::Error,
    },

    /// **The hm-xdp lesson.** An artifact's content hash does not match the
    /// manifest's pin: the corpus drifted under the harness. Never a warning.
    #[error("corpus hash mismatch for {what}: manifest pins {expected}, found {found}")]
    HashMismatch {
        /// The archive path or member name whose bytes changed.
        what: String,
        /// The sha256 the manifest pins.
        expected: String,
        /// The sha256 actually computed.
        found: String,
    },

    /// The manifest declares a schema version this build does not implement.
    /// Reading it with v1 semantics would silently ignore exactly the fields a
    /// later version added.
    #[error(
        "corpus manifest declares schema version {found}, but this build implements {expected}"
    )]
    ManifestVersion {
        /// The version the manifest declares.
        found: u32,
        /// The version this build implements.
        expected: u32,
    },

    /// The manifest names a path outside the corpus root. The manifest also
    /// supplies the hash each artifact is checked against, so content-addressing
    /// cannot enforce containment — only this can.
    #[error("corpus manifest path {path} escapes the corpus root {root}")]
    PathEscape {
        /// The offending path, as the manifest wrote it.
        path: String,
        /// The corpus root it must stay beneath.
        root: PathBuf,
    },

    /// An exclusion names a slice no loaded slice matches, so its member is
    /// never visited and its hash never checked. Silently skipping it would break
    /// the crate's core guarantee — that every excluded artifact is present and
    /// hash-checked — so a misspelled or stale exclusion is a loud failure.
    #[error(
        "exclusion for member {member} names slice {slice}, which matches no loaded slice \
         (expected exactly one)"
    )]
    UnknownExclusionSlice {
        /// The slice id the exclusion named.
        slice: String,
        /// The member the exclusion would have kept out.
        member: String,
    },

    /// Two trace entries share a `(slice, config, seed)` identity. Loading both
    /// would double-weight that campaign in every axis while each still passes its
    /// hash and ancestry checks — a silent scoring bias, so it is refused.
    #[error("duplicate campaign {config}/{seed} in slice {slice}: a seed is counted at most once")]
    DuplicateTrace {
        /// The slice the collision is in.
        slice: String,
        /// The campaign configuration (`Baseline` / `Signal`).
        config: String,
        /// The campaign seed.
        seed: u64,
    },

    /// The manifest names an archive member that the archive does not contain.
    #[error("archive {archive} has no member {member}")]
    MissingMember {
        /// The archive.
        archive: String,
        /// The member the manifest names.
        member: String,
    },

    /// A gzip / DEFLATE / ustar decoding failure.
    #[error("cannot decode archive {archive}: {why}")]
    Archive {
        /// The archive.
        archive: String,
        /// What the decoder objected to.
        why: String,
    },

    /// **Harness-correctness gate.** The v1-as-shipped candidate did not
    /// reproduce the campaign's recorded discovery events. The replay is wrong;
    /// never tune candidates against a broken replay (spec gate 2).
    #[error(
        "control replay diverged from the recorded campaign for {campaign} at branch {branch}: \
         recorded {recorded:?}, replayed {replayed:?}"
    )]
    ControlDiverged {
        /// The `(config, seed)` campaign.
        campaign: String,
        /// The branch at which the replayed cells first differ.
        branch: u64,
        /// The cell ids the campaign recorded.
        recorded: Vec<u64>,
        /// The cell ids the replay produced.
        replayed: Vec<u64>,
    },

    /// **Harness-correctness gate.** The campaign's selection stream did not
    /// reconstruct: a branch's replayed environment seed differs from the one the
    /// recorded trace carries, so the reconstructed ancestor chains would be
    /// fiction. Aborts rather than reporting an unfounded axis (c).
    #[error(
        "campaign replay diverged for {campaign} at branch {branch}: \
         recorded env seed {recorded:#x}, replayed {replayed:#x}"
    )]
    ChainDiverged {
        /// The `(config, seed)` campaign.
        campaign: String,
        /// The branch at which the replayed seed first differs.
        branch: u64,
        /// The seed the recorded trace's environment carries.
        recorded: u64,
        /// The seed the reconstructed selection stream produced.
        replayed: u64,
    },

    /// **Harness-correctness gate.** A reconstructed ancestor chain contradicts
    /// the `FindRecord` the campaign recorded for it.
    #[error(
        "reconstructed chain for {campaign} find at branch {branch} contradicts the record: \
         recorded (path_len {rec_path}, novel_on_path {rec_novel}), \
         reconstructed (path_len {got_path}, novel_on_path {got_novel})"
    )]
    ChainContradiction {
        /// The `(config, seed)` campaign.
        campaign: String,
        /// The finding branch.
        branch: u64,
        /// The `path_len` the campaign recorded.
        rec_path: u64,
        /// The `novel_on_path` the campaign recorded.
        rec_novel: u64,
        /// The `path_len` the reconstruction derived.
        got_path: u64,
        /// The `novel_on_path` the reconstruction derived.
        got_novel: u64,
    },

    /// A recorded trace could not be decoded into the vocabulary the harness
    /// folds: its environment is not an adapter blob, or a branch has no trace.
    #[error("corrupt corpus for {campaign}: {why}")]
    Corpus {
        /// The `(config, seed)` campaign.
        campaign: String,
        /// What the harness objected to.
        why: String,
    },
}
