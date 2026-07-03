// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`FaultPolicy`] — the per-fault-class eligibility and probability that
//! [`SeededEnv`](crate::SeededEnv) samples. Integer/fixed-point only; no floats
//! reach state (conventions rule 4).

use std::collections::BTreeMap;

use crate::Answer;
use crate::catalog::{DecisionClass, Fault};
use crate::codec::{self, Reader};
use crate::error::EnvError;
use crate::prng::Prng;

/// Container magic, `"FPL1"` read little-endian. Kept across the version bump
/// below — it is a container marker, not a version; the `VERSION` field gates the
/// vocabulary (exactly as [`EnvSpec`](crate::EnvSpec) keeps its `DEV2` magic while
/// `BLOB_VERSION` moved 2 → 3).
const MAGIC: u32 = u32::from_le_bytes(*b"FPL1");
/// The policy format version this build writes and decodes. Bumped to `2` by
/// task 50: a policy's `eligible` faults are encoded with the [`Fault`](crate::Fault)
/// byte tags, and the network tags were reshaped (per-frame → per-flow)
/// incompatibly. A task-45 `v1` blob stays byte-aligned under the new tags — e.g. a
/// payload-free old `NetDup` (tag 3) would silently decode as the new `NetReset`
/// (tag 3) — so [`from_bytes`](FaultPolicy::from_bytes) must reject `v1` with
/// [`EnvError::BadVersion`] rather than reinterpret it. This is the symmetric
/// codec to [`EnvSpec::BLOB_VERSION`](crate::EnvSpec::BLOB_VERSION): both the
/// reproducer blob and the standalone policy blob fail loudly on a stale net
/// vocabulary.
///
/// Bumped to `3` by task 73: the policy gained a trailing [`BuggifyPolicy`]
/// section (the per-point [`DecisionClass::Buggify`] biasing). A `v2` blob has
/// no such section, so a `v3` reader would run off the end of it (or a `v2`
/// reader would reject the trailing bytes); the version bump makes that an
/// explicit [`EnvError::BadVersion`] on either side rather than a truncation.
const VERSION: u16 = 3;

/// One fault class's policy: fault with probability `num/den` (a fixed-point
/// Bernoulli draw), and when it faults, pick uniformly from `eligible`. The
/// `eligible` list is canonical — strictly ascending, deduplicated, every fault
/// in the class — so its bytes are order-independent.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ClassPolicy {
    num: u32,
    den: u32,
    eligible: Vec<Fault>,
}

impl ClassPolicy {
    /// The never-fault policy (`0/1`, no eligible faults).
    fn none() -> Self {
        Self {
            num: 0,
            den: 1,
            eligible: Vec::new(),
        }
    }

    /// Draw one answer for a fault-class decision, advancing `rng` by exactly one
    /// word (so a fault-class decision advances the fault stream uniformly,
    /// independent of outcome). The single word decides both the Bernoulli trial
    /// (`w % den < num`) and, on a fault, the eligible index (`(w / den) % len`).
    /// `den ≥ 1` is an invariant of every constructor, so the modulo is safe.
    pub(crate) fn sample(&self, rng: &mut Prng) -> Answer {
        let w = rng.next_u64();
        let den = self.den as u64;
        let faulted = w % den < self.num as u64;
        if faulted && !self.eligible.is_empty() {
            let idx = ((w / den) % self.eligible.len() as u64) as usize;
            Answer::Fault(self.eligible[idx])
        } else {
            Answer::Nominal
        }
    }
}

/// The per-point [`DecisionClass::Buggify`] biasing (task 73): a default
/// fault probability `default_num/default_den` plus per-point overrides keyed by
/// the catalog-registered buggify site id. The guest never sees these numbers —
/// it asks `buggify(point)` and the host decides here, the deliberate
/// improvement over FoundationDB's anonymous `get_random`. Drawn from the
/// [`SeededEnv`](crate::SeededEnv) **fault** stream, never the supply stream.
///
/// A single fault-PRNG word decides the Bernoulli trial for each buggify point,
/// so enabling buggify shifts only the fault stream and leaves the guest's
/// entropy/payload supply byte-identical (the task-73 stream-separation gate).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct BuggifyPolicy {
    /// Default fire probability `num/den` for any point without an override.
    default_num: u32,
    default_den: u32,
    /// Per-point `(num, den)` overrides, keyed by site id — canonical
    /// (`BTreeMap`, sorted unique keys), `den >= 1` for every entry.
    per_point: BTreeMap<u32, (u32, u32)>,
}

impl BuggifyPolicy {
    /// The never-fire default (`0/1`, no per-point overrides).
    fn none() -> Self {
        Self {
            default_num: 0,
            default_den: 1,
            per_point: BTreeMap::new(),
        }
    }

    /// The `(num, den)` in force for `point` — its override, else the default.
    fn bias(&self, point: u32) -> (u32, u32) {
        self.per_point
            .get(&point)
            .copied()
            .unwrap_or((self.default_num, self.default_den))
    }

    /// Draw one buggify answer for `point`, advancing `rng` by exactly one word
    /// (so a buggify decision advances the fault stream uniformly, whatever the
    /// outcome). `den >= 1` is a constructor/decoder invariant, so the modulo is
    /// safe.
    fn sample(&self, point: u32, rng: &mut Prng) -> Answer {
        let (num, den) = self.bias(point);
        let den = den as u64;
        let w = rng.next_u64();
        let fires = w % den < num as u64;
        if fires {
            Answer::Fault(Fault::BuggifyFire)
        } else {
            Answer::Nominal
        }
    }
}

/// Per-class fault eligibility and probability, sampled by
/// [`SeededEnv`](crate::SeededEnv). Only the three fault classes
/// ([`NetFlow`](DecisionClass::NetFlow) / [`BlockIo`](DecisionClass::BlockIo) /
/// [`Process`](DecisionClass::Process)) carry a policy; the supply classes never
/// fault. The whole policy is part of a reproducer artifact (carried by every
/// [`EnvSpec`](crate::EnvSpec) variant), because a seed alone cannot reproduce a
/// campaign whose answer sequence depended on the eligible faults and
/// probabilities.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FaultPolicy {
    net: ClassPolicy,
    block: ClassPolicy,
    process: ClassPolicy,
    /// The per-point [`DecisionClass::Buggify`] biasing (task 73). Unlike the
    /// three class policies it is keyed per site, not per class.
    buggify: BuggifyPolicy,
}

impl FaultPolicy {
    /// The all-nominal baseline: every class never faults and no buggify point
    /// fires.
    pub fn none() -> Self {
        Self {
            net: ClassPolicy::none(),
            block: ClassPolicy::none(),
            process: ClassPolicy::none(),
            buggify: BuggifyPolicy::none(),
        }
    }

    /// Set one fault class's probability `num/den` and eligible faults.
    ///
    /// An addition to the spec's API (conventions rule 3): the only ergonomic
    /// way to build a non-baseline policy without going through
    /// [`from_bytes`](FaultPolicy::from_bytes). The `eligible` slice is
    /// canonicalized (sorted, deduplicated) so policy bytes never depend on its
    /// order. Returns [`EnvError::Malformed`] if `class` is a supply class
    /// (supply classes never fault), if `den == 0`, or if any eligible fault
    /// does not belong to `class`.
    pub fn set_class(
        &mut self,
        class: DecisionClass,
        num: u32,
        den: u32,
        eligible: &[Fault],
    ) -> Result<(), EnvError> {
        if !class.is_fault() || den == 0 {
            return Err(EnvError::Malformed);
        }
        for f in eligible {
            if f.class() != class {
                return Err(EnvError::Malformed);
            }
        }
        let mut e = eligible.to_vec();
        e.sort_unstable();
        e.dedup();
        let cp = ClassPolicy {
            num,
            den,
            eligible: e,
        };
        *self.class_mut(class) = cp;
        Ok(())
    }

    /// Set the **default** buggify fire probability `num/den` — the bias every
    /// [`DecisionClass::Buggify`] point uses unless it has a per-point override
    /// (task 73). Returns [`EnvError::Malformed`] if `den == 0`.
    pub fn set_buggify_default(&mut self, num: u32, den: u32) -> Result<(), EnvError> {
        if den == 0 {
            return Err(EnvError::Malformed);
        }
        self.buggify.default_num = num;
        self.buggify.default_den = den;
        Ok(())
    }

    /// Set the buggify fire probability `num/den` for one catalog-registered
    /// `point`, overriding the default for that point only (task 73). Returns
    /// [`EnvError::Malformed`] if `den == 0`.
    pub fn set_buggify_point(&mut self, point: u32, num: u32, den: u32) -> Result<(), EnvError> {
        if den == 0 {
            return Err(EnvError::Malformed);
        }
        self.buggify.per_point.insert(point, (num, den));
        Ok(())
    }

    /// Serialize to a byte-deterministic blob. Equal policies always yield
    /// identical bytes (fixed class order, canonical eligible lists, the
    /// buggify per-point map walked in ascending key order).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Vec::new();
        codec::put_u32(&mut w, MAGIC);
        codec::put_u16(&mut w, VERSION);
        for cp in [&self.net, &self.block, &self.process] {
            codec::put_u32(&mut w, cp.num);
            codec::put_u32(&mut w, cp.den);
            codec::put_len(&mut w, cp.eligible.len());
            for f in &cp.eligible {
                codec::write_fault(&mut w, f);
            }
        }
        // Buggify section (task 73): default bias, then the per-point overrides
        // in ascending id order (the `BTreeMap` is already canonical, so no
        // insertion order reaches a byte).
        codec::put_u32(&mut w, self.buggify.default_num);
        codec::put_u32(&mut w, self.buggify.default_den);
        codec::put_len(&mut w, self.buggify.per_point.len());
        for (point, (num, den)) in &self.buggify.per_point {
            codec::put_u32(&mut w, *point);
            codec::put_u32(&mut w, *num);
            codec::put_u32(&mut w, *den);
        }
        w
    }

    /// Decode a blob from [`to_bytes`](FaultPolicy::to_bytes). Never panics;
    /// off-version is [`EnvError::BadVersion`], every other defect (bad magic,
    /// truncation, trailing bytes, `den == 0`, a foreign-class or non-canonical
    /// eligible fault) is [`EnvError::Malformed`].
    pub fn from_bytes(b: &[u8]) -> Result<Self, EnvError> {
        let mut r = Reader::new(b);
        if r.u32()? != MAGIC {
            return Err(EnvError::Malformed);
        }
        let v = r.u16()?;
        if v != VERSION {
            return Err(EnvError::BadVersion(v));
        }
        let net = read_class(&mut r, DecisionClass::NetFlow)?;
        let block = read_class(&mut r, DecisionClass::BlockIo)?;
        let process = read_class(&mut r, DecisionClass::Process)?;
        let buggify = read_buggify(&mut r)?;
        if !r.at_end() {
            return Err(EnvError::Malformed);
        }
        Ok(Self {
            net,
            block,
            process,
            buggify,
        })
    }

    /// Draw one answer for a fault-class `class`, advancing `rng`. A supply class
    /// (never passed by [`SeededEnv`](crate::SeededEnv)) yields a defensive
    /// [`Answer::Nominal`] without drawing.
    pub(crate) fn sample(&self, class: DecisionClass, rng: &mut Prng) -> Answer {
        match class {
            DecisionClass::NetFlow => self.net.sample(rng),
            DecisionClass::BlockIo => self.block.sample(rng),
            DecisionClass::Process => self.process.sample(rng),
            _ => Answer::Nominal,
        }
    }

    /// Draw one answer for a [`DecisionClass::Buggify`] `point`, advancing the
    /// caller's fault `rng` by one word. Called by
    /// [`SeededEnv`](crate::SeededEnv) for a
    /// [`DecisionPoint::Buggify`](crate::DecisionPoint::Buggify) — from the
    /// **fault** stream, so buggify sampling never disturbs the guest's supply
    /// stream (task 73).
    pub(crate) fn sample_buggify(&self, point: u32, rng: &mut Prng) -> Answer {
        self.buggify.sample(point, rng)
    }

    /// Whether this policy faults **only** via buggify — every service class
    /// ([`NetFlow`](DecisionClass::NetFlow) / [`BlockIo`](DecisionClass::BlockIo)
    /// / [`Process`](DecisionClass::Process)) is the never-fault baseline, and
    /// only the per-point [`DecisionClass::Buggify`] biasing may be set (task 73).
    ///
    /// The task-73 SDK decide-seam enforces buggify (the guest asks over
    /// [`ServiceId::Sdk`], the host answers via [`decide`](crate::Environment::decide)),
    /// so a control server that has not yet built the full task-61 guest-fault
    /// enforcement loop can still **accept** a buggify-only reproducer — while a
    /// policy carrying an unenforced net/block/process fault is still rejected.
    pub fn is_buggify_only(&self) -> bool {
        self.net == ClassPolicy::none()
            && self.block == ClassPolicy::none()
            && self.process == ClassPolicy::none()
    }

    /// Mutable access to a fault class's policy. The caller guarantees `class` is
    /// a fault class (checked in [`set_class`](FaultPolicy::set_class)).
    fn class_mut(&mut self, class: DecisionClass) -> &mut ClassPolicy {
        match class {
            DecisionClass::BlockIo => &mut self.block,
            DecisionClass::Process => &mut self.process,
            // NetFlow (and, unreachably, supply classes) land here.
            _ => &mut self.net,
        }
    }
}

/// Decode one [`ClassPolicy`], enforcing `den ≥ 1`, every fault in `class`, and
/// a strictly-ascending (canonical, deduplicated) eligible list.
fn read_class(r: &mut Reader, class: DecisionClass) -> Result<ClassPolicy, EnvError> {
    let num = r.u32()?;
    let den = r.u32()?;
    if den == 0 {
        return Err(EnvError::Malformed);
    }
    let count = r.u32()?;
    let mut eligible: Vec<Fault> = Vec::new();
    for _ in 0..count {
        let f = codec::read_fault(r)?;
        if f.class() != class {
            return Err(EnvError::Malformed);
        }
        if eligible.last().is_some_and(|prev| f <= *prev) {
            return Err(EnvError::Malformed);
        }
        eligible.push(f);
    }
    Ok(ClassPolicy { num, den, eligible })
}

/// Decode the buggify section (task 73): a default `num/den` then the per-point
/// overrides. Enforces `den >= 1` on the default and every override, and a
/// strictly-ascending (canonical, deduplicated) per-point key order — so a
/// hand-crafted blob with a zero denominator, or an out-of-order/duplicate
/// point, is rejected rather than silently divided-by-zero or collapsed.
fn read_buggify(r: &mut Reader) -> Result<BuggifyPolicy, EnvError> {
    let default_num = r.u32()?;
    let default_den = r.u32()?;
    if default_den == 0 {
        return Err(EnvError::Malformed);
    }
    let count = r.u32()?;
    let mut per_point: BTreeMap<u32, (u32, u32)> = BTreeMap::new();
    let mut prev: Option<u32> = None;
    for _ in 0..count {
        let point = r.u32()?;
        if prev.is_some_and(|p| point <= p) {
            return Err(EnvError::Malformed);
        }
        prev = Some(point);
        let num = r.u32()?;
        let den = r.u32()?;
        if den == 0 {
            return Err(EnvError::Malformed);
        }
        per_point.insert(point, (num, den));
    }
    Ok(BuggifyPolicy {
        default_num,
        default_den,
        per_point,
    })
}
