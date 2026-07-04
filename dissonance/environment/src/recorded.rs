// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reproducer: [`EnvSpec`] (the serialized blob the control transport
//! carries as an opaque environment) and [`RecordedEnv`] (the
//! [`decide`](Environment::decide) backing it materializes into — a seeded base
//! plus sparse, admissibility-guarded *guest* overrides).
//!
//! This is the dissonance ruling's one [`Moment`]-keyed reproducer: a
//! `BTreeMap<Moment, Action>` carries host- and guest-plane overrides on the
//! single retired-instruction axis. Guest overrides ([`Action::Guest`]) are
//! answered at the [`decide`](Environment::decide) seam; host overrides
//! ([`Action::Host`]) are applied imperatively by the frontier at their
//! `Moment`, exactly like a [`StandingFault`] — they never flow through
//! [`decide`](Environment::decide).

use std::collections::BTreeMap;

use crate::catalog::{Answer, DecisionClass, DecisionPoint};
use crate::codec::{self, Reader};
use crate::error::EnvError;
use crate::host::{Action, HostFault, Moment};
use crate::policy::FaultPolicy;
use crate::seeded::SeededEnv;
use crate::{Environment, Outcome, VTime};

/// Container magic, `"DEV2"` read little-endian. Bumped from `DEV1` (task 24)
/// because the recorded value type widened from a guest `Answer` to an
/// [`Action`] keyed by [`Moment`] — a blob from the old layout no longer
/// decodes, and the magic makes that an explicit, loud rejection.
const MAGIC: u32 = u32::from_le_bytes(*b"DEV2");

/// A correlated, V-time-windowed fault that is **not** a per-`Moment`
/// [`Action`] — e.g. a network partition (a link and a window where all traffic
/// drops together). It is part of the reproducer so a `Branch`/`Replay`
/// re-applies it deterministically: the frontier hands each entry to the guest
/// utility, which **enforces** it on the intra-guest CNI for the window (e.g. an
/// nftables rule), exactly as it enforces a per-flow [`NetFlow`](DecisionClass::NetFlow)
/// answer — there is no host switch to consult (task 50 retired `pv-net`). It is
/// applied imperatively by the frontier, never through
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

/// The serialized reproducer — the dissonance ruling's `Environment { seed,
/// overrides }`, here an enum so the all-seed campaign (no overrides) is an
/// explicit, smaller blob. Both variants carry the [`FaultPolicy`]: a seed alone
/// cannot reproduce a campaign whose answer sequence depended on the eligible
/// faults and probabilities.
///
/// > **Naming.** The ruling overloads `Environment` for *both* the
/// > [`decide`](Environment::decide) seam (a trait) and this reproducer (a
/// > struct). Task 24 resolved the clash by keeping the trait as `Environment`
/// > and naming the reproducer `EnvSpec`; this amendment keeps that resolution
/// > and only widens the recorded value type (`Answer` → [`Action`]) and re-keys
/// > the map (decision index → [`Moment`]).
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
    /// decisions; the explorer's sparse, per-`Moment` `overrides` pin the
    /// interesting faults from *either* plane; `standing` carries the correlated
    /// V-time-windowed faults the frontier re-applies imperatively.
    Recorded {
        /// The base seed.
        seed: u64,
        /// The fault policy.
        policy: FaultPolicy,
        /// Per-`Moment` overrides, host and guest on one axis. A
        /// [`BTreeMap`](std::collections::BTreeMap) so the map is inherently
        /// canonical — sorted, unique keys — and no insertion order can reach
        /// an encoded byte.
        ///
        /// **Merged-plane seam.** The value type is [`Action`] = [`Host`](Action::Host)
        /// ∪ [`Guest`](Action::Guest), keyed by [`Moment`]: a host perturbation and
        /// a guest [`Answer`] share one ordered timeline. This widened from the
        /// task-24 guest-only `Answer` value when the host control plane landed
        /// (task 45) — so the widening task 46's clarifying pass anticipated as a
        /// forward-compat note is already realized here, not pending.
        overrides: BTreeMap<Moment, Action>,
        /// Correlated, V-time-windowed faults.
        standing: Vec<StandingFault>,
        /// The **reseed-marker table** (task 78): the sequential-entropy
        /// reseeds this reproducer's timeline carries, keyed by the [`Moment`]
        /// each took effect (a branch origin), valued by the seed
        /// (`SeededEntropy::new(seed)`). A compose-folded env re-executes each
        /// collapsed hop's reseed at its recorded position instead of
        /// reseeding once at the fold's root — the ruled fix for PR #58's
        /// sequential-entropy-splice finding. A `BTreeMap` (integer keys, no
        /// floats) so the table is inherently canonical and no insertion
        /// order can reach an encoded byte.
        reseeds: BTreeMap<Moment, u64>,
    },
}

impl EnvSpec {
    /// The reproducer blob format version. Bumps when the blob layout changes
    /// **or** when an inner byte vocabulary changes incompatibly;
    /// [`decode`](EnvSpec::decode) rejects any other version with
    /// [`EnvError::BadVersion`]. Bumped to `3` by task 50: the container layout
    /// (magic, [`Action`] map, standing faults) is unchanged, but the network
    /// [`Fault`](crate::Fault) byte vocabulary was reshaped (per-frame → per-flow),
    /// so a task-45 `v2` blob carrying an old net fault must reject rather than
    /// silently reinterpret it as a new flow policy. Bumped to `4` by task 78: the
    /// [`Recorded`](EnvSpec::Recorded) layout gained a trailing **reseed-marker
    /// table**, so a v3 blob (no table) rejects rather than mis-parse. Bumped to
    /// `5` by task 73: the embedded [`FaultPolicy`](crate::FaultPolicy) gained a
    /// trailing **buggify section** (its own version moved `2 → 3`). Task 78's `v4`
    /// embeds `FaultPolicy` v2 and task 73's embeds v3 — two **incompatible** inner
    /// encodings of the same logical policy (v3 is longer), so they must NOT share
    /// an outer version. A `v4` blob is therefore rejected outright at the version
    /// gate in [`decode`](EnvSpec::decode), never parsed with the v5 policy reader.
    pub const BLOB_VERSION: u16 = 5;

    /// The seed every backing draws from.
    pub fn seed(&self) -> u64 {
        match self {
            Self::Seeded { seed, .. } | Self::Recorded { seed, .. } => *seed,
        }
    }

    /// The fault policy carried by this spec.
    pub fn policy(&self) -> &FaultPolicy {
        match self {
            Self::Seeded { policy, .. } | Self::Recorded { policy, .. } => policy,
        }
    }

    /// The `Moment`-keyed overrides (empty for [`Seeded`](EnvSpec::Seeded)). The
    /// merged host+guest timeline the Progression manipulates uniformly.
    pub fn overrides(&self) -> &BTreeMap<Moment, Action> {
        match self {
            Self::Recorded { overrides, .. } => overrides,
            Self::Seeded { .. } => {
                // A process-wide empty map; `Seeded` has no overrides, so a
                // shared empty borrow is correct and allocation-free.
                static EMPTY: BTreeMap<Moment, Action> = BTreeMap::new();
                &EMPTY
            }
        }
    }

    /// The reseed-marker table (empty for [`Seeded`](EnvSpec::Seeded)): the
    /// sequential-entropy reseeds this reproducer carries, keyed by the
    /// [`Moment`] each took effect (a collapsed branch origin), valued by the
    /// seed. The frontier re-executes each at its `Moment` on `branch` — see
    /// the [`Recorded`](EnvSpec::Recorded) field doc.
    pub fn reseeds(&self) -> &BTreeMap<Moment, u64> {
        match self {
            Self::Recorded { reseeds, .. } => reseeds,
            Self::Seeded { .. } => {
                // A process-wide empty map; `Seeded` has no reseed markers, so
                // a shared empty borrow is correct and allocation-free.
                static EMPTY: BTreeMap<Moment, u64> = BTreeMap::new();
                &EMPTY
            }
        }
    }

    /// Stamp a reseed marker at `at`: the entropy stream was reseeded to
    /// `SeededEntropy::new(seed)` at that [`Moment`] (a branch origin).
    /// Promotes a [`Seeded`](EnvSpec::Seeded) spec to
    /// [`Recorded`](EnvSpec::Recorded) on first use; a later stamp at the same
    /// `Moment` overwrites (last write wins), matching [`record`](EnvSpec::record).
    pub fn record_reseed(&mut self, at: Moment, seed: u64) {
        self.promote();
        match self {
            Self::Recorded { reseeds, .. } => {
                reseeds.insert(at, seed);
            }
            // Unreachable: `promote` converted any `Seeded` to `Recorded`.
            Self::Seeded { .. } => unreachable!("Seeded was just promoted to Recorded"),
        }
    }

    /// Every host-plane perturbation, in `Moment` order — the frontier enforces
    /// these imperatively at each `Moment` during a run (they never reach
    /// [`decide`](Environment::decide)). The guest-plane half is consumed via
    /// [`materialize`](EnvSpec::materialize).
    pub fn host_faults(&self) -> impl Iterator<Item = (Moment, HostFault)> + '_ {
        self.overrides()
            .iter()
            .filter_map(|(m, a)| a.host_fault().map(|f| (*m, f)))
    }

    /// Stamp `action` at `at` on the single [`Moment`] axis — the uniform
    /// recording primitive for *both* planes (a guest decision at the count it
    /// surfaced, a host fault at the chosen count). Promotes a
    /// [`Seeded`](EnvSpec::Seeded) spec to [`Recorded`](EnvSpec::Recorded) on
    /// first use; a later stamp at the same `Moment` overwrites (last write
    /// wins), so the map stays one-action-per-`Moment`.
    pub fn record(&mut self, at: Moment, action: Action) {
        self.overrides_mut().insert(at, action);
    }

    /// Stage a host-plane [`HostFault`] at `at`, recorded into this environment —
    /// the recording half of the control transport's `perturb(fault, moment)`
    /// verb (the transport adds the wire/`ControlError` semantics in
    /// `control-proto`). Convenience for `record(at, Action::Host(fault))`.
    pub fn perturb(&mut self, fault: HostFault, at: Moment) {
        self.record(at, Action::Host(fault));
    }

    /// Promote a [`Seeded`](EnvSpec::Seeded) spec into an empty
    /// [`Recorded`](EnvSpec::Recorded) one in place (no-op if already
    /// `Recorded`).
    fn promote(&mut self) {
        if let Self::Seeded { seed, policy } = self {
            *self = Self::Recorded {
                seed: *seed,
                policy: policy.clone(),
                overrides: BTreeMap::new(),
                standing: Vec::new(),
                reseeds: BTreeMap::new(),
            };
        }
    }

    /// `&mut` access to the override map, promoting a [`Seeded`](EnvSpec::Seeded)
    /// spec into an empty [`Recorded`](EnvSpec::Recorded) one in place.
    fn overrides_mut(&mut self) -> &mut BTreeMap<Moment, Action> {
        self.promote();
        match self {
            Self::Recorded { overrides, .. } => overrides,
            // Unreachable: the block above converted any `Seeded` to `Recorded`.
            Self::Seeded { .. } => unreachable!("Seeded was just promoted to Recorded"),
        }
    }

    /// Serialize to a versioned, byte-deterministic blob. Overrides are written
    /// in `Moment` order (the `BTreeMap` is already canonical) and standing
    /// faults in canonical (sorted) order, so equal specs always yield identical
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
                reseeds,
            } => {
                w.push(1);
                codec::put_u64(&mut w, *seed);
                codec::put_bytes(&mut w, &policy.to_bytes());

                // The map is inherently canonical (sorted, unique keys), so a
                // plain iteration emits ascending `Moment`s and the
                // strictly-ascending round-trip on decode holds.
                codec::put_len(&mut w, overrides.len());
                for (m, action) in overrides {
                    codec::put_u64(&mut w, *m);
                    codec::put_bytes(&mut w, &action.encode());
                }

                // Deduplicate standing faults by their full canonical key
                // (identical entries collapse) and emit them in ascending-key
                // order, so input order cannot reach the bytes.
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

                // The reseed-marker table (task 78), ascending `Moment`s — the
                // map is inherently canonical, like the override map above.
                codec::put_len(&mut w, reseeds.len());
                for (m, seed) in reseeds {
                    codec::put_u64(&mut w, *m);
                    codec::put_u64(&mut w, *seed);
                }
            }
        }
        w
    }

    /// Decode a blob from [`encode`](EnvSpec::encode). Never panics on arbitrary
    /// or mutated bytes; off-version (including a task-24 `DEV1` blob, whose
    /// magic differs) is [`EnvError::BadVersion`] or [`EnvError::Malformed`],
    /// and every other defect (bad magic, truncation, trailing bytes, an unknown
    /// variant/plane/class tag, non-ascending/duplicate `Moment`s or standing
    /// faults) is [`EnvError::Malformed`].
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
                let reseeds = read_reseeds(&mut r)?;
                if !r.at_end() {
                    return Err(EnvError::Malformed);
                }
                Ok(Self::Recorded {
                    seed,
                    policy,
                    overrides,
                    standing,
                    reseeds,
                })
            }
            _ => Err(EnvError::Malformed),
        }
    }

    /// Materialize an [`Environment`] backing for the [`decide`](Environment::decide)
    /// seam. Only the **guest** overrides ([`Action::Guest`]) enter the
    /// [`RecordedEnv`]; **host** overrides ([`Action::Host`]) and standing faults
    /// are applied imperatively by the frontier (read via
    /// [`host_faults`](EnvSpec::host_faults) / [`StandingFault`]), so they are
    /// not part of `decide`.
    pub fn materialize(&self) -> RecordedEnv {
        let mut guest: BTreeMap<Moment, Answer> = BTreeMap::new();
        for (m, action) in self.overrides() {
            if let Some(ans) = action.guest_answer() {
                guest.insert(*m, ans.clone());
            }
        }
        RecordedEnv::new(self.seed(), self.policy().clone(), guest)
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

/// Read the per-`Moment` overrides into a canonical map, requiring
/// strictly-ascending `Moment`s (so a hand-crafted blob with a duplicate or
/// out-of-order key — which `encode` never emits — is rejected, not silently
/// collapsed).
fn read_overrides(r: &mut Reader) -> Result<BTreeMap<Moment, Action>, EnvError> {
    let n = r.u32()?;
    let mut overrides: BTreeMap<Moment, Action> = BTreeMap::new();
    let mut prev: Option<Moment> = None;
    for _ in 0..n {
        let m = r.u64()?;
        if prev.is_some_and(|p| m <= p) {
            return Err(EnvError::Malformed);
        }
        prev = Some(m);
        let action = Action::decode(r.bytes()?)?;
        overrides.insert(m, action);
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

/// Read the reseed-marker table, requiring strictly-ascending `Moment`s (so a
/// hand-crafted blob with a duplicate or out-of-order key — which `encode`
/// never emits — is rejected, not silently collapsed).
fn read_reseeds(r: &mut Reader) -> Result<BTreeMap<Moment, u64>, EnvError> {
    let n = r.u32()?;
    let mut reseeds: BTreeMap<Moment, u64> = BTreeMap::new();
    let mut prev: Option<Moment> = None;
    for _ in 0..n {
        let m = r.u64()?;
        if prev.is_some_and(|p| m <= p) {
            return Err(EnvError::Malformed);
        }
        prev = Some(m);
        let seed = r.u64()?;
        reseeds.insert(m, seed);
    }
    Ok(reseeds)
}

/// Answers a guest decision from a [`Moment`]-keyed override first, else from the
/// seeded base.
///
/// The frontier sets the current [`Moment`] (retired-instruction count) with
/// [`set_moment`](RecordedEnv::set_moment) before a guest decision surfaces, so
/// the right override fires at the right count — the same matching the real
/// reactive session did, now on the one `Moment` axis rather than a branch-local
/// decision index. An override whose [`Answer`] is **inadmissible for the
/// decision** is deterministically ignored (the seeded base answers instead), so
/// a mutated or hostile reproducer can never hand a service an impossible answer
/// or panic [`decide`](Environment::decide) (conventions rule 4); see
/// [`DecisionPoint::admits`]. The base stream advances **only on a fallback** (an
/// admissible override consumes no PRNG), exactly as a recorded reactive session
/// did, so replay is bit-identical.
#[derive(Clone, Debug)]
pub struct RecordedEnv {
    base: SeededEnv,
    overrides: BTreeMap<Moment, Answer>,
    moment: Moment,
}

impl RecordedEnv {
    /// Build from a seeded base and a guest-override map keyed by [`Moment`].
    fn new(seed: u64, policy: FaultPolicy, overrides: BTreeMap<Moment, Answer>) -> Self {
        Self {
            base: SeededEnv::new(seed, policy),
            overrides,
            moment: 0,
        }
    }

    /// Set the current [`Moment`] the next [`decide`](Environment::decide) is
    /// answering for. The frontier calls this before surfacing each guest
    /// decision (it knows the retired-instruction count); the count is what a
    /// `Moment`-keyed override matches against. Defaults to `0` until first set;
    /// a [`Seeded`](EnvSpec::Seeded)-materialized env has no overrides, so the
    /// `Moment` is irrelevant for it.
    pub fn set_moment(&mut self, at: Moment) {
        self.moment = at;
    }

    /// The current [`Moment`].
    pub fn moment(&self) -> Moment {
        self.moment
    }

    /// Serialize the **dynamic stream state** (the seeded base's PRNG positions)
    /// so a snapshot can resume the exact same supply and fault streams (task
    /// 73). The `Moment`-keyed overrides are static (part of the reproducer), so
    /// only the base stream position is captured. Delegates to
    /// [`SeededEnv::stream_state`].
    pub fn stream_state(&self) -> [u8; 16] {
        self.base.stream_state()
    }

    /// Restore the dynamic stream state captured by
    /// [`stream_state`](RecordedEnv::stream_state). Total (never panics).
    pub fn restore_stream_state(&mut self, state: &[u8; 16]) {
        self.base.restore_stream_state(state);
    }
}

impl Environment for RecordedEnv {
    fn decide(&mut self, point: &DecisionPoint) -> Outcome {
        if let Some(ans) = self.overrides.get(&self.moment)
            && point.admits(ans)
        {
            return Outcome::Resolved(ans.clone());
        }
        // An absent or inadmissible override falls through to the seeded base
        // (which advances its stream).
        Outcome::Resolved(self.base.answer(point))
    }
}
