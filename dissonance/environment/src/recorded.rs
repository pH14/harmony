// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reproducer: [`EnvSpec`] (the serialized blob R2 carries as an opaque
//! environment) and [`RecordedEnv`] (the backing it materializes into — a seeded
//! base plus sparse, admissibility-guarded overrides).

use std::collections::BTreeMap;

use crate::catalog::{Answer, DecisionClass, DecisionPoint};
use crate::codec::{self, Reader};
use crate::error::EnvError;
use crate::policy::FaultPolicy;
use crate::seeded::SeededEnv;
use crate::{DecisionId, Environment, Outcome, VTime};

/// Container magic, `"DEV1"` read little-endian.
const MAGIC: u32 = u32::from_le_bytes(*b"DEV1");

/// A correlated, V-time-windowed fault that is **not** a per-decision
/// [`Answer`] — e.g. a network partition (a link and a window where all frames
/// drop together). It is part of the reproducer so a `Branch`/`Replay`
/// re-applies it deterministically: the frontier translates each entry into the
/// service's standing-fault API (e.g. pv-net `Switch::set_partition`) on branch.
/// It is applied imperatively by the frontier, never through
/// [`decide`](Environment::decide) and never armed out-of-band where it would
/// escape replay. `target` is service-interpreted (it encodes, e.g., the
/// `(NodeId, NodeId)` link); its bytes are deterministic and no `HashMap` order
/// reaches them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StandingFault {
    /// The class this standing fault perturbs.
    pub class: DecisionClass,
    /// Opaque, service-interpreted target (e.g. an encoded link).
    pub target: Vec<u8>,
    /// The half-open V-time window `[start, end)` it applies over.
    pub window: (VTime, VTime),
}

/// The serialized reproducer. Both variants carry the [`FaultPolicy`]: a seed
/// alone cannot reproduce a campaign whose answer sequence depended on the
/// eligible faults and probabilities.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EnvSpec {
    /// Pure DST: seed plus policy, no overrides.
    Seeded {
        /// The base seed.
        seed: u64,
        /// The fault policy.
        policy: FaultPolicy,
    },
    /// A recorded reactive session: the seed auto-answers the high-frequency
    /// decisions; the explorer's sparse, per-decision `overrides` pin the
    /// interesting faults; `standing` carries the correlated V-time-windowed
    /// faults the frontier re-applies imperatively.
    Recorded {
        /// The base seed.
        seed: u64,
        /// The fault policy.
        policy: FaultPolicy,
        /// Per-decision overrides, keyed by [`DecisionId`]. Order is irrelevant:
        /// [`encode`](EnvSpec::encode) and [`materialize`](EnvSpec::materialize)
        /// both deduplicate by id (last write wins) and sort, so the blob is
        /// always canonical. A decoded spec therefore always has unique,
        /// strictly-ascending ids.
        overrides: Vec<(DecisionId, Answer)>,
        /// Correlated, V-time-windowed faults.
        standing: Vec<StandingFault>,
    },
}

impl EnvSpec {
    /// The reproducer blob format version. Bumps when the blob layout changes;
    /// [`decode`](EnvSpec::decode) rejects any other version with
    /// [`EnvError::BadVersion`].
    pub const BLOB_VERSION: u16 = 1;

    /// Serialize to a versioned, byte-deterministic blob. Overrides and standing
    /// faults are written in canonical (sorted) order, so equal specs — even
    /// with their `Vec`s built in different orders — always yield identical
    /// bytes (no iteration order reaches a byte).
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Vec::new();
        codec::put_u32(&mut w, MAGIC);
        codec::put_u16(&mut w, Self::BLOB_VERSION);
        match self {
            Self::Seeded { seed, policy } => {
                w.push(0);
                codec::put_u64(&mut w, *seed);
                codec::put_bytes(&mut w, &policy.to_bytes());
            }
            Self::Recorded {
                seed,
                policy,
                overrides,
                standing,
            } => {
                w.push(1);
                codec::put_u64(&mut w, *seed);
                codec::put_bytes(&mut w, &policy.to_bytes());

                // Deduplicate overrides by id (last write wins, identically to
                // `materialize`'s `BTreeMap`) and emit them in ascending id
                // order. Deduping on encode keeps the blob canonical: a duplicate
                // id can never reach the bytes, where it would break the
                // strictly-ascending round-trip on decode.
                let mut ov: BTreeMap<u64, &Answer> = BTreeMap::new();
                for (id, ans) in overrides {
                    ov.insert(id.0, ans);
                }
                codec::put_len(&mut w, ov.len());
                for (id, ans) in &ov {
                    codec::put_u64(&mut w, *id);
                    codec::put_bytes(&mut w, &ans.encode());
                }

                // Deduplicate standing faults by their full canonical key
                // (identical entries collapse) and emit them in ascending-key
                // order, for the same round-trip reason.
                let mut st: Vec<&StandingFault> = standing.iter().collect();
                st.sort_by(|a, b| standing_key(a).cmp(&standing_key(b)));
                st.dedup_by(|a, b| standing_key(a) == standing_key(b));
                codec::put_len(&mut w, st.len());
                for s in st {
                    codec::put_u16(&mut w, s.class.as_u16());
                    codec::put_bytes(&mut w, &s.target);
                    codec::put_u64(&mut w, s.window.0.0);
                    codec::put_u64(&mut w, s.window.1.0);
                }
            }
        }
        w
    }

    /// Decode a blob from [`encode`](EnvSpec::encode). Never panics on arbitrary
    /// or mutated bytes; off-version is [`EnvError::BadVersion`], every other
    /// defect (bad magic, truncation, trailing bytes, an unknown variant or
    /// class tag, non-ascending/duplicate overrides or standing faults) is
    /// [`EnvError::Malformed`].
    pub fn decode(b: &[u8]) -> Result<Self, EnvError> {
        let mut r = Reader::new(b);
        if r.u32()? != MAGIC {
            return Err(EnvError::Malformed);
        }
        let v = r.u16()?;
        if v != Self::BLOB_VERSION {
            return Err(EnvError::BadVersion(v));
        }
        let variant = r.u8()?;
        let seed = r.u64()?;
        let policy = FaultPolicy::from_bytes(r.bytes()?)?;

        match variant {
            0 => {
                if !r.at_end() {
                    return Err(EnvError::Malformed);
                }
                Ok(Self::Seeded { seed, policy })
            }
            1 => {
                let overrides = read_overrides(&mut r)?;
                let standing = read_standing(&mut r)?;
                if !r.at_end() {
                    return Err(EnvError::Malformed);
                }
                Ok(Self::Recorded {
                    seed,
                    policy,
                    overrides,
                    standing,
                })
            }
            _ => Err(EnvError::Malformed),
        }
    }

    /// Materialize an [`Environment`] backing. Standing faults are not part of
    /// [`decide`](Environment::decide) (the frontier applies them imperatively),
    /// so they are read off this [`EnvSpec`] directly and do not enter the
    /// [`RecordedEnv`]. Duplicate override ids collapse (last in the `Vec`
    /// wins); a decoded spec never has duplicates.
    pub fn materialize(&self) -> RecordedEnv {
        match self {
            Self::Seeded { seed, policy } => {
                RecordedEnv::new(*seed, policy.clone(), BTreeMap::new())
            }
            Self::Recorded {
                seed,
                policy,
                overrides,
                standing: _,
            } => {
                let mut map = BTreeMap::new();
                for (id, ans) in overrides {
                    map.insert(id.0, ans.clone());
                }
                RecordedEnv::new(*seed, policy.clone(), map)
            }
        }
    }
}

/// The canonical sort key for a [`StandingFault`].
fn standing_key(s: &StandingFault) -> (u16, &[u8], u64, u64) {
    (
        s.class.as_u16(),
        s.target.as_slice(),
        s.window.0.0,
        s.window.1.0,
    )
}

/// Read the per-decision overrides, requiring strictly-ascending ids.
fn read_overrides(r: &mut Reader) -> Result<Vec<(DecisionId, Answer)>, EnvError> {
    let n = r.u32()?;
    let mut overrides: Vec<(DecisionId, Answer)> = Vec::new();
    for _ in 0..n {
        let id = r.u64()?;
        if overrides.last().is_some_and(|(prev, _)| id <= prev.0) {
            return Err(EnvError::Malformed);
        }
        let ans = Answer::decode(r.bytes()?)?;
        overrides.push((DecisionId(id), ans));
    }
    Ok(overrides)
}

/// Read the standing faults, requiring strictly-ascending canonical keys.
fn read_standing(r: &mut Reader) -> Result<Vec<StandingFault>, EnvError> {
    let m = r.u32()?;
    let mut standing: Vec<StandingFault> = Vec::new();
    let mut prev: Option<(u16, Vec<u8>, u64, u64)> = None;
    for _ in 0..m {
        let class = DecisionClass::from_u16(r.u16()?).ok_or(EnvError::Malformed)?;
        let target = r.bytes()?.to_vec();
        let w0 = r.u64()?;
        let w1 = r.u64()?;
        let key = (class.as_u16(), target.clone(), w0, w1);
        if prev.as_ref().is_some_and(|p| key <= *p) {
            return Err(EnvError::Malformed);
        }
        prev = Some(key);
        standing.push(StandingFault {
            class,
            target,
            window: (VTime(w0), VTime(w1)),
        });
    }
    Ok(standing)
}

/// Answers from overrides first, else falls back to the seeded base, recording
/// the decision index so the right override applies at the right decision.
///
/// An override whose [`Answer`] is **inadmissible for the decision** is
/// deterministically ignored — the seeded base answers instead — so a mutated or
/// hostile [`EnvSpec`] can never hand a service an impossible answer or panic
/// [`decide`](Environment::decide) (conventions rule 4). See
/// [`DecisionPoint::admits`] for the exact rule. The base stream advances only on
/// a fallback (an admissible override consumes no PRNG), exactly as a recorded
/// reactive session did, so replay is bit-identical.
#[derive(Clone, Debug)]
pub struct RecordedEnv {
    base: SeededEnv,
    overrides: BTreeMap<u64, Answer>,
    counter: u64,
}

impl RecordedEnv {
    /// Build from a seeded base and an override map keyed by decision index.
    fn new(seed: u64, policy: FaultPolicy, overrides: BTreeMap<u64, Answer>) -> Self {
        Self {
            base: SeededEnv::new(seed, policy),
            overrides,
            counter: 0,
        }
    }
}

impl Environment for RecordedEnv {
    fn decide(&mut self, point: &DecisionPoint) -> Outcome {
        let id = self.counter;
        self.counter = self.counter.wrapping_add(1);
        if let Some(ans) = self.overrides.get(&id)
            && point.admits(ans)
        {
            return Outcome::Resolved(ans.clone());
        }
        // An absent or inadmissible override falls through to the seeded base
        // (which advances its stream).
        Outcome::Resolved(self.base.answer(point))
    }
}
