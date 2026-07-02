// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crate error and the content-addressed [`TraceId`].

use std::fmt;

use explorer::Environment;

/// A **content address** for a run: `blake3` of the run's canonical environment
/// bytes ([`crate::codec::encode_env`]). Because a run is a pure function of its
/// `Environment` and the encoding is canonical, byte-stability of the reproducer
/// *is* id-stability — two determinism-identical runs share a `TraceId` for
/// free, and two divergent runs never collide (task 65 §1).
///
/// Rendered/parsed as lowercase hex (the on-disk sidecar/journal filenames);
/// `Ord` so [`TraceStore::ids`](crate::TraceStore::ids) can iterate
/// deterministically.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TraceId(pub [u8; 32]);

impl TraceId {
    /// The content address of `env`: `blake3` over its canonical bytes.
    pub fn of(env: &Environment) -> TraceId {
        TraceId(*blake3::hash(&crate::codec::encode_env(env)).as_bytes())
    }

    /// Lowercase-hex rendering (64 chars) — the sidecar/journal filename stem.
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            // Two lowercase hex digits per byte; `write!` to a String is
            // infallible, but the no-`unwrap` rule prefers explicit pushes.
            s.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
            s.push(char::from_digit((b & 0xf) as u32, 16).unwrap_or('0'));
        }
        s
    }

    /// Parse a 64-char lowercase-hex id (a sidecar filename stem); `None` if it
    /// is not exactly 32 hex-encoded bytes.
    pub fn from_hex(s: &str) -> Option<TraceId> {
        let bytes = s.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            let hi = (bytes[2 * i] as char).to_digit(16)?;
            let lo = (bytes[2 * i + 1] as char).to_digit(16)?;
            out[i] = ((hi << 4) | lo) as u8;
            i += 1;
        }
        Some(TraceId(out))
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for TraceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TraceId({})", self.to_hex())
    }
}

/// Every fallible outcome in this crate. Library code never panics on untrusted
/// input (conventions rule 4): a malformed journal, an unknown format version,
/// or a missing/env-only trace is a loud, typed error — never a silent
/// reinterpretation.
#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    /// The journal's [`TRACE_FORMAT_VERSION`](crate::TRACE_FORMAT_VERSION) is not
    /// the one this build understands. **Never a silent reinterpretation** — an
    /// on-disk format is versioned from day one (task 65 §1, gate 4), so a bump
    /// fails loudly here rather than decoding old fields with new meaning.
    #[error("unknown trace format version {found} (this build understands {supported})")]
    Version {
        /// The version read from the journal header.
        found: u16,
        /// The version this build encodes/decodes.
        supported: u16,
    },
    /// The journal did not start with the expected magic — not a RunTrace journal.
    #[error("not a RunTrace journal (bad magic)")]
    Magic,
    /// The journal ended mid-field (or a length ran past the buffer). Bounds are
    /// checked against the *actual* buffer before any read (control-proto
    /// discipline), so this is a clean error, never an out-of-bounds panic.
    #[error("truncated or malformed trace journal")]
    Truncated,
    /// The journal had trailing bytes after a complete decode — a
    /// non-canonical encoding, rejected to keep `encode(decode(b)) == b`.
    #[error("trailing bytes after a complete trace journal")]
    Trailing,
    /// A map-shaped field was not in canonical form — a `BTreeMap`-backed
    /// collection (an event's `attrs`) whose keys are not **strictly
    /// increasing**. Rejected loudly rather than silently re-sorted/deduped,
    /// which would break `encode(decode(b)) == b` for the accepted bytes.
    #[error("non-canonical trace journal (map keys not strictly increasing)")]
    NonCanonical,
    /// A string field (an event kind/key, a `Value::Str`) was not valid UTF-8.
    #[error("non-UTF-8 string field in trace journal")]
    Utf8,
    /// The [`TraceStore`](crate::TraceStore) hit a filesystem error.
    #[error("trace store I/O error")]
    Io(#[from] std::io::Error),
    /// No trace with this id is present in the store (neither env nor journal).
    #[error("no trace {0} in store")]
    NotFound(TraceId),
    /// The env sidecar is present but the full journal was not retained
    /// (recorded under [`Retain::EnvOnly`](crate::Retain::EnvOnly)); the run
    /// regenerates by replay from its env (task 65 §3).
    #[error("trace {0} was recorded env-only; its journal is not retained")]
    NotRetained(TraceId),
    /// A telemetry NDJSON line could not be parsed while ingesting a `Console`
    /// recording ([`crate::ingest_ndjson`]).
    #[error("malformed telemetry NDJSON while ingesting a Console recording: {0}")]
    Ingest(String),
}
