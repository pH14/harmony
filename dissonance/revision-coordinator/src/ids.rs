// SPDX-License-Identifier: AGPL-3.0-or-later
//! Identity and terminal-record types for the revision coordinator.
//!
//! Every id is a `u64` newtype with a total, seed-derived order (Convention
//! rule 4): the coordinator mints [`ProposalId`]s, [`Revision`]s, and
//! [`CohortId`]s densely from 1 in seeded issue order, so ordering by value IS
//! ordering by issue. [`EvidenceBatchId`] and [`CampaignConfigId`] are opaque
//! digest-based identities minted elsewhere (`hm-bbx.4` / the campaign
//! config); the coordinator never inspects their contents.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Monotonic Differential logical timestamp. The ONLY timestamp; never
/// wall-clock (`docs/DISSONANCE-STRATEGY.md` doctrine). The coordinator mints
/// revisions densely from 1 in seeded issue order; [`Revision::ZERO`] is the
/// empty-frontier sentinel (nothing committed / nothing visible), never a
/// mintable slot.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct Revision(u64);

impl Revision {
    /// The empty frontier: no revision minted, committed, or visible.
    pub const ZERO: Revision = Revision(0);

    /// Wrap a raw revision number.
    pub fn new(value: u64) -> Self {
        Revision(value)
    }

    /// The raw revision number.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Identity of a proposal persisted before dispatch (seeded issue order).
/// Minted densely from 1; a crashed worker retries the SAME `ProposalId`.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct ProposalId(u64);

impl ProposalId {
    /// Wrap a raw proposal number.
    pub fn new(value: u64) -> Self {
        ProposalId(value)
    }

    /// The raw proposal number.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// A frozen cohort: fixed selector/archive view, canonical proposal mint
/// order. Minted densely from 1 by [`Coordinator::open_cohort`]; the frozen
/// view is the search-visible frontier at open, and the cohort's results
/// become visible only after it is closed and every member has committed.
///
/// [`Coordinator::open_cohort`]: crate::Coordinator::open_cohort
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct CohortId(u64);

impl CohortId {
    /// Wrap a raw cohort number.
    pub fn new(value: u64) -> Self {
        CohortId(value)
    }

    /// The raw cohort number.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Opaque, already-durable evidence-batch identity supplied by `hm-bbx.4`.
///
/// Digest-based (32 bytes); the coordinator commits it to a [`Revision`]
/// without decoding the payload it names. Serialized as a lowercase hex
/// string so ledger records and golden projections stay readable.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvidenceBatchId(pub(crate) [u8; 32]);

impl EvidenceBatchId {
    /// Wrap an existing 32-byte digest.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        EvidenceBatchId(bytes)
    }

    /// The raw digest bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Mint an identity as the BLAKE3 digest of an already-durable batch
    /// encoding. Convenience for tests and for `hm-bbx.4`'s ledger bridge;
    /// the coordinator itself never derives or inspects batch identities.
    pub fn digest(payload: &[u8]) -> Self {
        EvidenceBatchId(*blake3::hash(payload).as_bytes())
    }
}

impl std::fmt::Debug for EvidenceBatchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EvidenceBatchId({})", hex_encode(&self.0))
    }
}

/// Content-addressed identity of the immutable campaign configuration this
/// coordinator orders proposals under (the sealed-campaign boundary). Bound
/// at [`Coordinator::genesis`] and pinned by the ledger's genesis record; the
/// coordinator never decodes it.
///
/// [`Coordinator::genesis`]: crate::Coordinator::genesis
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CampaignConfigId(pub(crate) [u8; 32]);

impl CampaignConfigId {
    /// Wrap an existing 32-byte digest.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        CampaignConfigId(bytes)
    }

    /// The raw digest bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Mint an identity as the BLAKE3 digest of a canonical config encoding.
    pub fn digest(payload: &[u8]) -> Self {
        CampaignConfigId(*blake3::hash(payload).as_bytes())
    }
}

impl std::fmt::Debug for CampaignConfigId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CampaignConfigId({})", hex_encode(&self.0))
    }
}

/// The deterministic V-time/work terminal record that closes a successful
/// issued revision (`docs/DISSONANCE-STRATEGY.md`: every issued revision slot
/// "must end in a deterministic terminal record under V-time/work limits").
/// Both coordinates are integers on the deterministic axis; a retried worker
/// must reproduce them exactly, so a divergent retry is a determinism bug and
/// is refused as a commit conflict.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
pub struct TerminalRecord {
    /// Terminal `Moment` (deterministic V-time coordinate) of the rollout.
    pub moment: u64,
    /// Deterministic work counter at the terminal.
    pub work: u64,
}

/// Lowercase hex encoding (no external `hex` dependency).
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
    out
}

/// Decode exactly 64 lowercase/uppercase hex digits into 32 bytes.
pub(crate) fn hex_decode32(s: &str) -> Option<[u8; 32]> {
    let bytes = s.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let digit = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut out = [0u8; 32];
    for (i, chunk) in bytes.chunks_exact(2).enumerate() {
        out[i] = (digit(chunk[0])? << 4) | digit(chunk[1])?;
    }
    Some(out)
}

macro_rules! hex_serde {
    ($ty:ident) => {
        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&hex_encode(&self.0))
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                hex_decode32(&s)
                    .map($ty)
                    .ok_or_else(|| D::Error::custom(concat!(stringify!($ty), ": not 64 hex digits")))
            }
        }
    };
}

hex_serde!(EvidenceBatchId);
hex_serde!(CampaignConfigId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let id = EvidenceBatchId::digest(b"batch");
        let json = serde_json::to_string(&id).unwrap();
        let back: EvidenceBatchId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
        assert_eq!(json.len(), 66); // 64 digits + quotes
    }

    #[test]
    fn hex_decode_rejects_bad_input() {
        assert!(hex_decode32("zz").is_none());
        assert!(hex_decode32(&"g".repeat(64)).is_none());
        assert!(hex_decode32(&"a".repeat(63)).is_none());
        assert!(hex_decode32(&"a".repeat(64)).is_some());
    }

    #[test]
    fn ids_order_by_value() {
        assert!(Revision::new(1) < Revision::new(2));
        assert!(Revision::ZERO < Revision::new(1));
        assert_eq!(ProposalId::new(7).get(), 7);
        assert_eq!(CohortId::new(3).get(), 3);
    }
}
