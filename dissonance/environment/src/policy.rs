// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`FaultPolicy`] — the per-fault-class eligibility and probability that
//! [`SeededEnv`](crate::SeededEnv) samples. Integer/fixed-point only; no floats
//! reach state (conventions rule 4).

use crate::Answer;
use crate::catalog::{DecisionClass, Fault};
use crate::codec::{self, Reader};
use crate::error::EnvError;
use crate::prng::Prng;

/// Container magic, `"FPL1"` read little-endian.
const MAGIC: u32 = u32::from_le_bytes(*b"FPL1");
/// The policy format version this build writes and decodes.
const VERSION: u16 = 1;

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
}

impl FaultPolicy {
    /// The all-nominal baseline: every class never faults.
    pub fn none() -> Self {
        Self {
            net: ClassPolicy::none(),
            block: ClassPolicy::none(),
            process: ClassPolicy::none(),
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

    /// Serialize to a byte-deterministic blob. Equal policies always yield
    /// identical bytes (fixed class order, canonical eligible lists).
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
        if !r.at_end() {
            return Err(EnvError::Malformed);
        }
        Ok(Self {
            net,
            block,
            process,
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
