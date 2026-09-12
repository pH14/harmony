// SPDX-License-Identifier: AGPL-3.0-or-later

mod catalog;
mod codec;
mod envcodec;
mod error;
mod host;
mod policy;
mod prng;
mod process;
mod recorded;
mod seeded;
mod standing;

pub use catalog::{Answer, BlockOp, DecisionClass, DecisionPoint, Fault, FlowEvent};
pub use envcodec::EnvCodec;
pub use error::EnvError;
pub use host::{Action, BitMask, HostFault, Moment, Ratio};
pub use policy::FaultPolicy;
pub use process::{decode_process_target, process_target};
pub use recorded::{EnvSpec, RecordedEnv, StandingFault};
pub use seeded::SeededEnv;
pub use standing::{
    STANDING_NAMESPACE, StandingEntry, StandingIter, StandingWindow, decode_windows,
    encode_standing, encode_windows, parse_standing,
};

pub const CATALOG_VERSION: u16 = 5;

pub const MAX_SUPPLY_LEN: u32 = 1 << 20;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    Resolved(Answer),
    NeedsHost,
}

pub trait Environment {
    fn decide(&mut self, point: &DecisionPoint) -> Outcome;
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct NodeId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct ConnId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Span(pub u64);

pub mod consonance;
