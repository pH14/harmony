// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::Span;
use crate::catalog::Answer;
use crate::codec::{self, Reader};
use crate::error::EnvError;

pub type Moment = u64;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Ratio {
    num: u64,
    den: u64,
}

impl Ratio {
    pub fn new(num: u64, den: u64) -> Option<Self> {
        (den != 0).then_some(Self { num, den })
    }

    pub fn num(self) -> u64 {
        self.num
    }

    pub fn den(self) -> u64 {
        self.den
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct BitMask(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum HostFault {
    SkewTime(Span),
    SetClockRate(Ratio),
    CorruptMemory { gpa: u64, mask: BitMask },
    InjectInterrupt { vector: u32 },
}

impl HostFault {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Vec::new();
        codec::write_host_fault(&mut w, self);
        w
    }

    pub fn decode(b: &[u8]) -> Result<Self, EnvError> {
        let mut r = Reader::new(b);
        let f = codec::read_host_fault(&mut r)?;
        if !r.at_end() {
            return Err(EnvError::Malformed);
        }
        Ok(f)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Host(HostFault),
    Guest(Answer),
}

impl Action {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Vec::new();
        codec::write_action(&mut w, self);
        w
    }

    pub fn decode(b: &[u8]) -> Result<Self, EnvError> {
        let mut r = Reader::new(b);
        let a = codec::read_action(&mut r)?;
        if !r.at_end() {
            return Err(EnvError::Malformed);
        }
        Ok(a)
    }

    pub fn host_fault(&self) -> Option<HostFault> {
        match self {
            Self::Host(f) => Some(*f),
            Self::Guest(_) => None,
        }
    }

    pub fn guest_answer(&self) -> Option<&Answer> {
        match self {
            Self::Guest(a) => Some(a),
            Self::Host(_) => None,
        }
    }
}

impl From<HostFault> for Action {
    fn from(f: HostFault) -> Self {
        Self::Host(f)
    }
}

impl From<Answer> for Action {
    fn from(a: Answer) -> Self {
        Self::Guest(a)
    }
}
