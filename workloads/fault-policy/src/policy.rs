// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use crate::Answer;
use crate::catalog::{DecisionClass, Fault};
use crate::codec::{self, Reader};
use crate::error::EnvError;
use crate::prng::Prng;

const MAGIC: u32 = u32::from_le_bytes(*b"FPL1");
const VERSION: u16 = 3;

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ClassPolicy {
    num: u32,
    den: u32,
    eligible: Vec<Fault>,
}

impl ClassPolicy {
    fn none() -> Self {
        Self {
            num: 0,
            den: 1,
            eligible: Vec::new(),
        }
    }

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

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct BuggifyPolicy {
    default_num: u32,
    default_den: u32,
    per_point: BTreeMap<u32, (u32, u32)>,
}

impl BuggifyPolicy {
    fn none() -> Self {
        Self {
            default_num: 0,
            default_den: 1,
            per_point: BTreeMap::new(),
        }
    }

    fn bias(&self, point: u32) -> (u32, u32) {
        self.per_point
            .get(&point)
            .copied()
            .unwrap_or((self.default_num, self.default_den))
    }

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

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FaultPolicy {
    net: ClassPolicy,
    block: ClassPolicy,
    process: ClassPolicy,
    buggify: BuggifyPolicy,
}

impl FaultPolicy {
    pub fn none() -> Self {
        Self {
            net: ClassPolicy::none(),
            block: ClassPolicy::none(),
            process: ClassPolicy::none(),
            buggify: BuggifyPolicy::none(),
        }
    }

    pub fn set_class(
        &mut self,
        class: DecisionClass,
        num: u32,
        den: u32,
        eligible: &[Fault],
    ) -> Result<(), EnvError> {
        if !class.is_fault() || den == 0 || class == DecisionClass::Buggify {
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

    pub fn set_buggify_default(&mut self, num: u32, den: u32) -> Result<(), EnvError> {
        if den == 0 {
            return Err(EnvError::Malformed);
        }
        self.buggify.default_num = num;
        self.buggify.default_den = den;
        Ok(())
    }

    pub fn set_buggify_point(&mut self, point: u32, num: u32, den: u32) -> Result<(), EnvError> {
        if den == 0 {
            return Err(EnvError::Malformed);
        }
        self.buggify.per_point.insert(point, (num, den));
        Ok(())
    }

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

    pub(crate) fn sample(&self, class: DecisionClass, rng: &mut Prng) -> Answer {
        match class {
            DecisionClass::NetFlow => self.net.sample(rng),
            DecisionClass::BlockIo => self.block.sample(rng),
            DecisionClass::Process => self.process.sample(rng),
            _ => Answer::Nominal,
        }
    }

    pub(crate) fn sample_buggify(&self, point: u32, rng: &mut Prng) -> Answer {
        self.buggify.sample(point, rng)
    }

    pub fn is_buggify_only(&self) -> bool {
        self.net == ClassPolicy::none()
            && self.block == ClassPolicy::none()
            && self.process == ClassPolicy::none()
    }

    pub fn is_enforceable_only(&self) -> bool {
        self.block == ClassPolicy::none()
            && self.process == ClassPolicy::none()
            && self.net.eligible.iter().all(|f| !is_fractional_loss(f))
    }

    fn class_mut(&mut self, class: DecisionClass) -> &mut ClassPolicy {
        match class {
            DecisionClass::BlockIo => &mut self.block,
            DecisionClass::Process => &mut self.process,
            _ => &mut self.net,
        }
    }
}

fn is_fractional_loss(fault: &Fault) -> bool {
    matches!(fault, Fault::NetLoss { num, den } if *den > 0 && num < den)
}

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
