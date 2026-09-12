// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use crate::catalog::{Answer, DecisionClass, DecisionPoint};
use crate::codec::{self, Reader};
use crate::error::EnvError;
use crate::host::{Action, HostFault, Moment};
use crate::policy::FaultPolicy;
use crate::seeded::SeededEnv;
use crate::{Environment, Outcome};

const MAGIC: u32 = u32::from_le_bytes(*b"DEV2");

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StandingFault {
    pub class: DecisionClass,
    pub target: Vec<u8>,
    pub window: (Moment, Moment),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EnvSpec {
    Seeded {
        seed: u64,
        policy: FaultPolicy,
    },
    Recorded {
        seed: u64,
        policy: FaultPolicy,
        overrides: BTreeMap<Moment, Action>,
        standing: Vec<StandingFault>,
        reseeds: BTreeMap<Moment, u64>,
        payloads: Option<Vec<Vec<u8>>>,
    },
}

impl EnvSpec {
    pub const BLOB_VERSION: u16 = 7;

    pub fn seed(&self) -> u64 {
        match self {
            Self::Seeded { seed, .. } | Self::Recorded { seed, .. } => *seed,
        }
    }

    pub fn policy(&self) -> &FaultPolicy {
        match self {
            Self::Seeded { policy, .. } | Self::Recorded { policy, .. } => policy,
        }
    }

    pub fn overrides(&self) -> &BTreeMap<Moment, Action> {
        match self {
            Self::Recorded { overrides, .. } => overrides,
            Self::Seeded { .. } => {
                static EMPTY: BTreeMap<Moment, Action> = BTreeMap::new();
                &EMPTY
            }
        }
    }

    pub fn reseeds(&self) -> &BTreeMap<Moment, u64> {
        match self {
            Self::Recorded { reseeds, .. } => reseeds,
            Self::Seeded { .. } => {
                static EMPTY: BTreeMap<Moment, u64> = BTreeMap::new();
                &EMPTY
            }
        }
    }

    pub fn payloads(&self) -> Option<&[Vec<u8>]> {
        match self {
            Self::Recorded { payloads, .. } => payloads.as_deref(),
            Self::Seeded { .. } => None,
        }
    }

    pub fn set_payloads(&mut self, payloads: Option<Vec<Vec<u8>>>) {
        if payloads.is_some() {
            self.promote();
        }
        if let Self::Recorded { payloads: p, .. } = self {
            *p = payloads;
        }
    }

    pub fn record_reseed(&mut self, at: Moment, seed: u64) {
        self.promote();
        match self {
            Self::Recorded { reseeds, .. } => {
                reseeds.insert(at, seed);
            }
            Self::Seeded { .. } => unreachable!("Seeded was just promoted to Recorded"),
        }
    }

    pub fn host_faults(&self) -> impl Iterator<Item = (Moment, HostFault)> + '_ {
        self.overrides()
            .iter()
            .filter_map(|(m, a)| a.host_fault().map(|f| (*m, f)))
    }

    pub fn record(&mut self, at: Moment, action: Action) {
        self.overrides_mut().insert(at, action);
    }

    pub fn perturb(&mut self, fault: HostFault, at: Moment) {
        self.record(at, Action::Host(fault));
    }

    fn promote(&mut self) {
        if let Self::Seeded { seed, policy } = self {
            *self = Self::Recorded {
                seed: *seed,
                policy: policy.clone(),
                overrides: BTreeMap::new(),
                standing: Vec::new(),
                reseeds: BTreeMap::new(),
                payloads: None,
            };
        }
    }

    fn overrides_mut(&mut self) -> &mut BTreeMap<Moment, Action> {
        self.promote();
        match self {
            Self::Recorded { overrides, .. } => overrides,
            Self::Seeded { .. } => unreachable!("Seeded was just promoted to Recorded"),
        }
    }

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
                payloads,
            } => {
                w.push(1);
                codec::put_u64(&mut w, *seed);
                codec::put_bytes(&mut w, &policy.to_bytes());

                codec::put_len(&mut w, overrides.len());
                for (m, action) in overrides {
                    codec::put_u64(&mut w, *m);
                    codec::put_bytes(&mut w, &action.encode());
                }

                let mut st: Vec<&StandingFault> = standing.iter().collect();
                st.sort_by(|a, b| standing_key(a).cmp(&standing_key(b)));
                st.dedup_by(|a, b| standing_key(a) == standing_key(b));
                codec::put_len(&mut w, st.len());
                for s in st {
                    codec::put_u16(&mut w, s.class.as_u16());
                    codec::put_bytes(&mut w, &s.target);
                    codec::put_u64(&mut w, s.window.0);
                    codec::put_u64(&mut w, s.window.1);
                }

                codec::put_len(&mut w, reseeds.len());
                for (m, seed) in reseeds {
                    codec::put_u64(&mut w, *m);
                    codec::put_u64(&mut w, *seed);
                }

                match payloads {
                    None => w.push(0),
                    Some(entries) => {
                        w.push(1);
                        codec::put_len(&mut w, entries.len());
                        for entry in entries {
                            codec::put_bytes(&mut w, entry);
                        }
                    }
                }
            }
        }
        w
    }

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
                let payloads = match r.u8()? {
                    0 => None,
                    1 => {
                        let count = r.u32()?;
                        let mut entries = Vec::new();
                        for _ in 0..count {
                            entries.push(r.bytes()?.to_vec());
                        }
                        Some(entries)
                    }
                    _ => return Err(EnvError::Malformed),
                };
                if !r.at_end() {
                    return Err(EnvError::Malformed);
                }
                Ok(Self::Recorded {
                    seed,
                    policy,
                    overrides,
                    standing,
                    reseeds,
                    payloads,
                })
            }
            _ => Err(EnvError::Malformed),
        }
    }

    pub fn materialize(&self) -> RecordedEnv {
        let mut guest: BTreeMap<Moment, Answer> = BTreeMap::new();
        for (m, action) in self.overrides() {
            if let Some(ans) = action.guest_answer() {
                guest.insert(*m, ans.clone());
            }
        }
        RecordedEnv::new(
            self.seed(),
            self.policy().clone(),
            guest,
            self.payloads().map(<[Vec<u8>]>::to_vec),
        )
    }
}

fn standing_key(s: &StandingFault) -> (u16, &[u8], u64, u64) {
    (
        s.class.as_u16(),
        s.target.as_slice(),
        s.window.0,
        s.window.1,
    )
}

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
            window: (w0, w1),
        });
    }
    Ok(standing)
}

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

#[derive(Clone, Debug)]
pub struct RecordedEnv {
    base: SeededEnv,
    overrides: BTreeMap<Moment, Answer>,
    moment: Moment,
    payloads: Option<Vec<Vec<u8>>>,
    payload_cursor: usize,
}

impl RecordedEnv {
    fn new(
        seed: u64,
        policy: FaultPolicy,
        overrides: BTreeMap<Moment, Answer>,
        payloads: Option<Vec<Vec<u8>>>,
    ) -> Self {
        Self {
            base: SeededEnv::new(seed, policy),
            overrides,
            moment: 0,
            payloads,
            payload_cursor: 0,
        }
    }

    pub fn payload_configured(&self) -> bool {
        self.payloads.is_some()
    }

    pub fn pull_payload(&mut self, bytes: u32) -> Result<Option<Vec<u8>>, u32> {
        let Some(entries) = self.payloads.as_ref() else {
            return Ok(None);
        };
        let Some(entry) = entries.get(self.payload_cursor) else {
            return Ok(None);
        };
        let actual = u32::try_from(entry.len()).unwrap_or(u32::MAX);
        if actual != bytes {
            return Err(actual);
        }
        let value = entry.clone();
        self.payload_cursor += 1;
        Ok(Some(value))
    }

    pub fn remaining_payloads(&self) -> Option<Vec<Vec<u8>>> {
        self.payloads
            .as_ref()
            .map(|entries| entries[self.payload_cursor..].to_vec())
    }

    pub fn restore_payloads(&mut self, remaining: Option<Vec<Vec<u8>>>) {
        self.payloads = remaining;
        self.payload_cursor = 0;
    }

    pub fn set_moment(&mut self, at: Moment) {
        self.moment = at;
    }

    pub fn moment(&self) -> Moment {
        self.moment
    }

    pub fn stream_state(&self) -> [u8; 16] {
        self.base.stream_state()
    }

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
        Outcome::Resolved(self.base.answer(point))
    }
}
