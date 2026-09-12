// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::codec::{self, Reader};
use crate::error::EnvError;
use crate::{ConnId, NodeId, Span};

#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum DecisionClass {
    Entropy = 1,
    Payload = 2,
    Scheduler = 3,
    NetFlow = 4,
    BlockIo = 5,
    Process = 6,
    Buggify = 7,
}

impl DecisionClass {
    pub fn is_supply(self) -> bool {
        matches!(self, Self::Entropy | Self::Payload | Self::Scheduler)
    }

    pub fn is_fault(self) -> bool {
        !self.is_supply()
    }

    #[must_use]
    pub fn as_u16(self) -> u16 {
        self as u16
    }

    #[must_use]
    pub fn from_wire(v: u16) -> Option<Self> {
        Self::from_u16(v)
    }

    pub(crate) fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::Entropy),
            2 => Some(Self::Payload),
            3 => Some(Self::Scheduler),
            4 => Some(Self::NetFlow),
            5 => Some(Self::BlockIo),
            6 => Some(Self::Process),
            7 => Some(Self::Buggify),
            _ => None,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum BlockOp {
    Read = 0,
    Write = 1,
    Flush = 2,
}

#[repr(u16)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum FlowEvent {
    Open = 0,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DecisionPoint {
    Entropy {
        bytes: u32,
    },
    Payload {
        bytes: u32,
    },
    Scheduler {
        ready: u32,
    },
    NetFlow {
        src: NodeId,
        dst: NodeId,
        conn: ConnId,
        event: FlowEvent,
    },
    BlockIo {
        op: BlockOp,
        lba: u64,
        len: u32,
    },
    Process {
        node: NodeId,
    },
    Buggify {
        point: u32,
    },
}

impl DecisionPoint {
    pub fn class(&self) -> DecisionClass {
        match self {
            Self::Entropy { .. } => DecisionClass::Entropy,
            Self::Payload { .. } => DecisionClass::Payload,
            Self::Scheduler { .. } => DecisionClass::Scheduler,
            Self::NetFlow { .. } => DecisionClass::NetFlow,
            Self::BlockIo { .. } => DecisionClass::BlockIo,
            Self::Process { .. } => DecisionClass::Process,
            Self::Buggify { .. } => DecisionClass::Buggify,
        }
    }

    pub fn admits(&self, ans: &Answer) -> bool {
        match (self, ans) {
            (Self::Entropy { bytes }, Answer::Supply(v))
            | (Self::Payload { bytes }, Answer::Supply(v)) => v.len() as u64 == *bytes as u64,
            (Self::Scheduler { ready }, Answer::Supply(v)) => {
                v.len() == 4 && u32::from_le_bytes([v[0], v[1], v[2], v[3]]) < *ready
            }
            (
                Self::NetFlow { .. }
                | Self::BlockIo { .. }
                | Self::Process { .. }
                | Self::Buggify { .. },
                Answer::Nominal,
            ) => true,
            (
                Self::NetFlow { .. }
                | Self::BlockIo { .. }
                | Self::Process { .. }
                | Self::Buggify { .. },
                Answer::Fault(f),
            ) => f.class() == self.class() && self.fault_bounds_ok(f),
            _ => false,
        }
    }

    fn fault_bounds_ok(&self, fault: &Fault) -> bool {
        match (self, fault) {
            (Self::BlockIo { len, .. }, Fault::BlockTorn(n)) => *n as u64 <= *len as u64,
            _ => true,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Fault {
    NetLatency(Span),
    NetLoss { num: u16, den: u16 },
    NetThrottle { bps: u32 },
    NetReset,
    BlockEio,
    BlockLatency(Span),
    BlockTorn(u32),
    BlockNospc,
    ProcPause(Span),
    ProcKill,
    ProcRestart,
    BuggifyFire,
    RunHook(u32),
    ProcPark { addr: u64, hits: u32, hold: Span },
}

impl Fault {
    pub fn class(&self) -> DecisionClass {
        match self {
            Self::NetLatency(_)
            | Self::NetLoss { .. }
            | Self::NetThrottle { .. }
            | Self::NetReset => DecisionClass::NetFlow,
            Self::BlockEio | Self::BlockLatency(_) | Self::BlockTorn(_) | Self::BlockNospc => {
                DecisionClass::BlockIo
            }
            Self::ProcPause(_)
            | Self::ProcKill
            | Self::ProcRestart
            | Self::RunHook(_)
            | Self::ProcPark { .. } => DecisionClass::Process,
            Self::BuggifyFire => DecisionClass::Buggify,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Answer {
    Nominal,
    Supply(Vec<u8>),
    Fault(Fault),
}

impl Answer {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Vec::new();
        codec::write_answer(&mut w, self);
        w
    }

    pub fn decode(b: &[u8]) -> Result<Self, EnvError> {
        let mut r = Reader::new(b);
        let a = codec::read_answer(&mut r)?;
        if !r.at_end() {
            return Err(EnvError::Malformed);
        }
        Ok(a)
    }
}
